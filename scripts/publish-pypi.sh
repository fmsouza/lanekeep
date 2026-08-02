#!/usr/bin/env bash
# Publish the built wheels to PyPI, skipping any file already on the index.
#
# Run from the repository root, against what `build-python-wheels.sh` produced:
#
#   ./scripts/publish-pypi.sh dist-wheels
#
# Authentication comes from the environment, as twine expects it:
#
#   TWINE_USERNAME=__token__ TWINE_PASSWORD=pypi-... ./scripts/publish-pypi.sh dist-wheels
#
# # Resumability, and why it is per-file
#
# PyPI refuses to replace a file that already exists, so a release that dies partway cannot
# simply be re-run — the re-run stops on the first thing already uploaded and never reaches
# the one that failed. Both other lanes learned this the expensive way; see the note in
# `publish-npm.sh`.
#
# The check here is against the *filename*, not the version, and that distinction is the
# whole point. A partial upload leaves the version present on the index while two of its
# four wheels are missing, so asking "is 0.3.1 published?" answers yes and skips exactly the
# work that still needs doing. Asking "is this wheel published?" resumes correctly.
set -euo pipefail

dist="${1:?usage: publish-pypi.sh <wheels-dir> [--dry-run]}"
shift

dry_run=no
for argument in "$@"; do
  case "${argument}" in
  --dry-run) dry_run=yes ;;
  *)
    echo "error: unknown argument ${argument}" >&2
    exit 2
    ;;
  esac
done

# Overridable so the tests can drive stubs. Nothing else sets either.
twine_command="${TWINE:-twine}"
curl_command="${CURL:-curl}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

wheels=()
for wheel in "${dist}"/*.whl; do
  [ -f "${wheel}" ] || continue
  wheels+=("${wheel}")
done

# A release that uploads nothing is a release that silently has no Python distribution.
if [ "${#wheels[@]}" -eq 0 ]; then
  echo "error: no wheels found in ${dist}" >&2
  exit 1
fi

# The version each wheel claims, taken from its filename: lanekeep-<version>-py3-none-<tag>.whl
version_of() {
  local base
  base="$(basename "$1")"
  base="${base#lanekeep-}"
  printf '%s' "${base%%-*}"
}

version="$(version_of "${wheels[0]}")"
for wheel in "${wheels[@]}"; do
  if [ "$(version_of "${wheel}")" != "${version}" ]; then
    echo "error: ${dist} holds wheels for more than one version" >&2
    echo "       ${version} and $(version_of "${wheel}"), which cannot both be this release" >&2
    exit 1
  fi
done

# The same agreement `publish-npm.sh` insists on, for the same reason: the wheels learn the
# version from the tag and crates.io learns it from the workspace manifest, and if those
# disagree one release ships under two numbers. No index lets a number be reused, so it
# costs nothing to notice here and everything to notice afterwards.
workspace_version="$(python3 - "${repo_root}/Cargo.toml" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
section = text.split("[workspace.package]", 1)[1]
print(re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE).group(1))
PY
)"
# Trailing carriage return stripped: Python's text-mode stdout writes CRLF on Windows, and a
# version carrying a CR compares equal to nothing.
workspace_version="$(printf '%s' "${workspace_version}" | tr -d '\r')"

if [ "${version}" != "${workspace_version}" ]; then
  echo "error: PyPI would publish ${version}, but Cargo.toml says ${workspace_version}" >&2
  echo "       the tag and the workspace manifest have to agree before any index sees it" >&2
  exit 1
fi

# Every filename already on the index for this version, one per line. A version PyPI has
# never heard of is a 404, which is the answer we want rather than an error — so a failed
# request and an absent version are deliberately indistinguishable here, and both mean
# "nothing uploaded yet".
#
# Carriage returns are stripped for the same reason `workspace_version` above strips them,
# and it matters more here: these values are matched against filenames with `grep -Fxq`, so a
# trailing CR does not fail loudly — it just never matches, the skip list comes back empty,
# and every wheel is offered to the index again. `--skip-existing` would carry it, but the
# resumability this script exists for would be quietly gone.
published="$("${curl_command}" -sf "https://pypi.org/pypi/lanekeep/${version}/json" 2>/dev/null |
  python3 -c 'import json,sys
try:
    print("\n".join(u["filename"] for u in json.load(sys.stdin).get("urls", [])))
except Exception:
    pass' | tr -d '\r' || true)"

pending=()
for wheel in "${wheels[@]}"; do
  name="$(basename "${wheel}")"
  if printf '%s\n' "${published}" | grep -Fxq "${name}"; then
    echo "skip    ${name} (already published)"
    continue
  fi
  pending+=("${wheel}")
done

if [ "${#pending[@]}" -eq 0 ]; then
  echo "pypi: every wheel for ${version} is already published"
  exit 0
fi

# Fails on a malformed wheel or unrenderable metadata, which is the one class of upload
# error worth catching before it reaches a registry that cannot take the file back.
"${twine_command}" check ${pending[@]+"${pending[@]}"}

for wheel in "${pending[@]}"; do
  echo "publish $(basename "${wheel}")"
done

if [ "${dry_run}" = "yes" ]; then
  echo "pypi: dry run, ${#pending[@]} wheel(s) not uploaded"
  exit 0
fi

# `--skip-existing` on top of the filename check above, which is not redundant: the check
# describes the index as it was a moment ago, and a re-run racing another is exactly the
# situation where a version has been half-published. This turns that from a failed release
# into a no-op.
"${twine_command}" upload --non-interactive --skip-existing ${pending[@]+"${pending[@]}"}

echo "pypi: ${#pending[@]} wheel(s) uploaded for ${version}"
