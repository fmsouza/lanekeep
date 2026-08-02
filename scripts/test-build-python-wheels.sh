#!/usr/bin/env bash
# Tests for build-python-wheels.sh and the two Python helpers beside it.
#
# A malformed wheel fails at `pip install`, on someone else's machine, after the release has
# gone out — and PyPI does not let a file be replaced. Everything here is cheap; the failure
# it prevents is not.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/build-python-wheels.sh"

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

# --- fixtures --------------------------------------------------------------------------------
#
# The Linux slots hold real ELF bytes rather than stand-in text, because the build checks the
# glibc floor of anything it is about to tag `manylinux`, and refuses a file it cannot parse.
# That refusal is correct — a Linux artifact that is not an ELF is a broken release, not a
# thing to shrug at — so the fixture has to be the real shape.
#
# Minimal but genuine: a section table, a string table, and a `.gnu.version_r` naming one
# library and one version, which is the entire structure the checker reads.
cat >"${work}/make_elf.py" <<'PY'
"""Write a minimal ELF64 whose .gnu.version_r requires one glibc version."""
import struct
import sys

destination, required = sys.argv[1], sys.argv[2]

strings = b"\0" + b"libc.so.6\0" + required.encode() + b"\0"
lib_offset = 1
version_offset = 1 + len("libc.so.6") + 1

# One Verneed naming libc.so.6, with one Vernaux naming the version.
verneed = struct.pack("<HHIII", 1, 1, lib_offset, 16, 0)
verneed += struct.pack("<IHHII", 0, 0, 2, version_offset, 0)

names = b"\0.shstrtab\0.dynstr\0.gnu.version_r\0"
shstrtab_name = 1
dynstr_name = 1 + len(".shstrtab") + 1
verneed_name = dynstr_name + len(".dynstr") + 1

header_size = 64
section_count = 4

names_offset = header_size
strings_offset = names_offset + len(names)
verneed_offset = strings_offset + len(strings)
sections_offset = verneed_offset + len(verneed)

def section(name, type_, offset, size, link=0, info=0):
    return struct.pack("<IIQQQQIIQQ", name, type_, 0, 0, offset, size, link, info, 1, 0)

sections = section(0, 0, 0, 0)
sections += section(shstrtab_name, 3, names_offset, len(names))
sections += section(dynstr_name, 3, strings_offset, len(strings))
# SHT_GNU_verneed. sh_link points at .dynstr, sh_info is the Verneed count.
sections += section(verneed_name, 0x6FFFFFFE, verneed_offset, len(verneed), link=2, info=1)

header = b"\x7fELF" + bytes([2, 1, 1, 0]) + b"\0" * 8
header += struct.pack("<HHI", 2, 0x3E, 1)
header += struct.pack("<QQQ", 0, 0, sections_offset)
header += struct.pack("<IHHH", 0, header_size, 0, 0)
header += struct.pack("<HHH", 64, section_count, 1)

open(destination, "wb").write(header + names + strings + verneed + sections)
PY

# --- a stand-in for a release build's output ------------------------------------------------
artifacts="${work}/dist"
mkdir -p "${artifacts}/aarch64-apple-darwin"
printf 'not-a-real-binary-darwin' >"${artifacts}/aarch64-apple-darwin/lanekeep"
mkdir -p "${artifacts}/x86_64-pc-windows-msvc"
printf 'not-a-real-binary-windows' >"${artifacts}/x86_64-pc-windows-msvc/lanekeep.exe"
for triple in aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir -p "${artifacts}/${triple}"
  python3 "${work}/make_elf.py" "${artifacts}/${triple}/lanekeep" GLIBC_2.17
done

# `tr -d '\r'` on everything Python prints, here and below: its text-mode stdout writes CRLF
# on Windows, and a value carrying a CR compares equal to nothing.
version="$(python3 - "${repo_root}/Cargo.toml" <<'PY' | tr -d '\r'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
section = text.split("[workspace.package]", 1)[1]
print(re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE).group(1))
PY
)"

out="${work}/wheels"
"${script}" "${version}" "${artifacts}" "${out}" >/dev/null 2>&1
check "a complete build produces wheels" "0" "$?"

check "one wheel per platform" "4" \
  "$(find "${out}" -name '*.whl' | wc -l | tr -d ' ')"

# --- the filename is the platform contract ---------------------------------------------------
#
# pip picks a wheel by parsing its filename and nothing else. A wrong tag here is not a
# cosmetic problem: it is either an install that cannot resolve, or one that resolves to a
# binary for the wrong machine.
for tag in macosx_11_0_arm64 \
  manylinux_2_17_aarch64.manylinux2014_aarch64 \
  manylinux_2_17_x86_64.manylinux2014_x86_64 \
  win_amd64; do
  check "there is a wheel tagged ${tag}" "1" \
    "$([ -f "${out}/lanekeep-${version}-py3-none-${tag}.whl" ] && echo 1 || echo 0)"
done

darwin="${out}/lanekeep-${version}-py3-none-macosx_11_0_arm64.whl"
windows="${out}/lanekeep-${version}-py3-none-win_amd64.whl"

# --- the binary, where an installer will look for it ------------------------------------------
_inspect() {
  python3 - "$1" "$2" <<'PY'
import sys, zipfile
archive, query = sys.argv[1], sys.argv[2]
z = zipfile.ZipFile(archive)
if query == "names":
    print("\n".join(sorted(z.namelist())))
elif query.startswith("mode:"):
    info = z.getinfo(query[5:])
    print(oct((info.external_attr >> 16) & 0o7777)[2:])
elif query.startswith("read:"):
    sys.stdout.write(z.read(query[5:]).decode("utf-8"))
PY
}

inspect() {
  _inspect "$@" | tr -d '\r'
}

check "the binary is under .data/scripts" "1" \
  "$(inspect "${darwin}" names | grep -c "^lanekeep-${version}.data/scripts/lanekeep$")"
# The one bit that makes the wheel useful. `upload-artifact` strips permissions in transit,
# so the mode is written explicitly rather than copied from the file on disk — the npm lane
# shipped a 0644 binary once and every invocation died with EACCES.
check "and it is executable" "755" \
  "$(inspect "${darwin}" "mode:lanekeep-${version}.data/scripts/lanekeep")"
check "the windows wheel carries the .exe" "1" \
  "$(inspect "${windows}" names | grep -c "^lanekeep-${version}.data/scripts/lanekeep.exe$")"

# Nothing importable ships. A stray package directory would be installed into site-packages
# and shadow nothing useful, but it would make `import lanekeep` half-work, which is worse
# than it failing outright.
check "no importable module is shipped" "0" \
  "$(inspect "${darwin}" names | grep -c '\.py$')"

# --- metadata PyPI reads ------------------------------------------------------------------------
metadata="$(inspect "${darwin}" "read:lanekeep-${version}.dist-info/METADATA")"
check "the name is lanekeep" "1" "$(printf '%s' "${metadata}" | grep -c '^Name: lanekeep$')"
check "the version is the one asked for" "1" \
  "$(printf '%s' "${metadata}" | grep -c "^Version: ${version}$")"
check "the license is the workspace's" "1" \
  "$(printf '%s' "${metadata}" | grep -c '^License: MIT OR Apache-2.0$')"
check "the readme is the description" "1" \
  "$(printf '%s' "${metadata}" | grep -c '^Description-Content-Type: text/markdown$')"
# No dependencies at all, which is the property worth asserting rather than assuming: the
# binary carries its own JavaScript engine, so installing lanekeep must pull in nothing.
check "the wheel depends on nothing" "0" \
  "$(printf '%s' "${metadata}" | grep -c '^Requires-Dist:')"

wheel_file="$(inspect "${darwin}" "read:lanekeep-${version}.dist-info/WHEEL")"
# The load-bearing line. A `true` here would tell the installer the contents are portable
# Python and put a machine-specific binary in a location shared across platforms.
check "the wheel is marked platform-specific" "1" \
  "$(printf '%s' "${wheel_file}" | grep -c '^Root-Is-Purelib: false$')"
check "the WHEEL tag matches the filename" "1" \
  "$(printf '%s' "${wheel_file}" | grep -c '^Tag: py3-none-macosx_11_0_arm64$')"

check "both licenses travel with it" "2" \
  "$(inspect "${darwin}" names | grep -c 'dist-info/LICENSE-')"

# --- RECORD, which pip verifies on install ---------------------------------------------------
#
# A wrong hash is an install failure rather than something that degrades quietly, so this
# recomputes every one of them rather than checking the file merely exists.
check "every RECORD hash matches its entry" "ok" \
  "$(python3 - "${darwin}" <<'PY' | tr -d '\r'
import base64, csv, hashlib, io, sys, zipfile

z = zipfile.ZipFile(sys.argv[1])
record = next(n for n in z.namelist() if n.endswith(".dist-info/RECORD"))
rows = list(csv.reader(io.StringIO(z.read(record).decode())))

listed = set()
for name, digest, size in rows:
    listed.add(name)
    if name == record:
        if digest or size:
            print(f"RECORD lists itself with a hash: {digest!r} {size!r}")
            break
        continue
    payload = z.read(name)
    want = "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(payload).digest()).rstrip(b"=").decode()
    if digest != want:
        print(f"{name}: RECORD says {digest}, content hashes to {want}")
        break
    if int(size) != len(payload):
        print(f"{name}: RECORD says {size} bytes, content is {len(payload)}")
        break
else:
    missing = set(z.namelist()) - listed
    print("unlisted entries: " + ", ".join(sorted(missing)) if missing else "ok")
PY
)"

# --- the same input twice is the same bytes ---------------------------------------------------
#
# A wheel is a zip, and a zip records the mtime and umask of whatever built it unless told
# otherwise. Not a correctness problem on its own, but this project's whole claim is that
# identical input produces identical output, and a release artifact is a poor place to make
# an exception.
again="${work}/wheels-again"
"${script}" "${version}" "${artifacts}" "${again}" >/dev/null 2>&1
differing=0
for wheel in "${out}"/*.whl; do
  name="$(basename "${wheel}")"
  if ! cmp -s "${wheel}" "${again}/${name}"; then
    differing=$((differing + 1))
  fi
done
check "rebuilding produces identical wheels" "0" "${differing}"

# --- a missing platform fails loudly ------------------------------------------------------------
partial="${work}/partial"
mkdir -p "${partial}/aarch64-apple-darwin"
printf 'x' >"${partial}/aarch64-apple-darwin/lanekeep"
"${script}" "${version}" "${partial}" "${work}/partial-out" >/dev/null 2>&1
check "a missing platform fails the build" "1" "$?"

# --- the glibc floor check ---------------------------------------------------------------------
#
# The check standing between a manylinux tag and a promise it cannot keep, so it is tested
# against ELF bytes rather than trusted.
python3 "${work}/make_elf.py" "${work}/old.so" GLIBC_2.17
python3 "${work}/make_elf.py" "${work}/new.so" GLIBC_2.39

# A Linux artifact that is not an ELF at all is a broken release rather than a floor of zero.
printf 'this is not an ELF' >"${work}/text.so"
python3 "${repo_root}/scripts/check_glibc_floor.py" "${work}/text.so" 2.17 >/dev/null 2>&1
check "a file that is not an ELF fails" "1" "$?"

python3 "${repo_root}/scripts/check_glibc_floor.py" "${work}/old.so" 2.17 >/dev/null 2>&1
check "a binary at the claimed floor passes" "0" "$?"

python3 "${repo_root}/scripts/check_glibc_floor.py" "${work}/new.so" 2.17 >/dev/null 2>&1
check "a binary needing a newer glibc fails" "1" "$?"

# The message has to name the version, because "the floor is wrong" is not actionable and
# the fix depends on which one it is.
check "and the failure names the version needed" "1" \
  "$(python3 "${repo_root}/scripts/check_glibc_floor.py" "${work}/new.so" 2.17 2>&1 |
    grep -c 'needs glibc 2.39')"
check "and points at where the floor is pinned" "1" \
  "$(python3 "${repo_root}/scripts/check_glibc_floor.py" "${work}/new.so" 2.17 2>&1 |
    grep -c 'release.yml')"

# 2.9 versus 2.17 is the comparison a string sort gets backwards, and getting it backwards
# means rejecting a binary that was fine or accepting one that was not.
python3 "${repo_root}/scripts/check_glibc_floor.py" "${work}/old.so" 2.9 >/dev/null 2>&1
check "versions compare numerically, not as text" "1" "$?"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
