#!/usr/bin/env bash
# Assemble the Python wheels, from the binaries a release build produced.
#
# Run from the repository root:
#
#   ./scripts/build-python-wheels.sh 0.3.1 dist [output-dir]
#
# where `dist/` holds one directory per target triple, each containing the built binary.
# That is the same input `build-npm-packages.sh` and `build-release-archives.sh` take,
# deliberately: one build feeds every distribution channel, so what pip installs, what npm
# installs and what the releases page serves are the same bytes.
#
# # Why there is no launcher
#
# The npm distribution needs one — a package that inspects `process.platform` at run time and
# hands off to whichever platform package got installed. pip needs nothing of the sort,
# because a wheel names the platform it is for in its own filename and the installer picks by
# that tag. Four wheels, one project, no resolution logic to get wrong.
#
# # Why the binary goes under `.data/scripts/`
#
# That is the one directory in a wheel whose contents are installed onto `PATH` — into the
# environment's `bin/` (or `Scripts\` on Windows) — with the executable bit set. It is how
# every Rust CLI on PyPI ships, and it is why `lanekeep` needs no Python code at all: there
# is no console-script shim, no `__main__.py`, nothing importable. The wheel is a delivery
# mechanism for a binary and declares itself as such.
#
# # Determinism
#
# Entries are written sorted, with a fixed timestamp and explicit permissions, so the same
# input produces byte-identical wheels. A wheel is a zip, and a zip otherwise records the
# mtime and the umask of whatever machine built it.
set -euo pipefail

version="${1:?usage: build-python-wheels.sh <version> <artifacts-dir> [output-dir]}"
artifacts="${2:?usage: build-python-wheels.sh <version> <artifacts-dir> [output-dir]}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${3:-${repo_root}/dist-wheels}"

rm -rf "${out}"
mkdir -p "${out}"

# target triple : binary name : wheel platform tag
#
# The manylinux tags are a *claim about glibc*, and the only reason they are honest is that
# `release.yml` pins the floor when it builds — see the `--target ...gnu.2.17` there. Left to
# the runner image, the floor is whatever Ubuntu the label currently points at: v0.3.1 shipped
# needing glibc 2.39 because `ubuntu-latest` had become 24.04, which no tag here would have
# made true. A wheel that overstates its floor installs cleanly and then dies at exec time
# with a message about a version of a library the user has never heard of.
#
# The doubled `manylinux_2_17_x.manylinux2014_x` is one compressed tag set, not two tags: the
# PEP 600 name and the legacy PEP 599 alias for the same thing, so installers too old to know
# the first still match the second.
targets=(
  "aarch64-apple-darwin:lanekeep:macosx_11_0_arm64"
  "aarch64-unknown-linux-gnu:lanekeep:manylinux_2_17_aarch64.manylinux2014_aarch64"
  "x86_64-unknown-linux-gnu:lanekeep:manylinux_2_17_x86_64.manylinux2014_x86_64"
  "x86_64-pc-windows-msvc:lanekeep.exe:win_amd64"
)

built=0

for entry in "${targets[@]}"; do
  triple="${entry%%:*}"
  rest="${entry#*:}"
  binary="${rest%%:*}"
  tag="${rest##*:}"
  source="${artifacts}/${triple}/${binary}"

  if [ ! -f "${source}" ]; then
    # Loud, not silent, for the same reason the archive builder is: a platform missing from
    # PyPI is a platform whose users are told "no matching distribution", and nothing else
    # here would say so until one of them reported it.
    echo "error: no binary for ${triple} at ${source}" >&2
    exit 1
  fi

  # The manylinux tag is a promise about glibc, so it is checked against the binary rather
  # than trusted. See `check_glibc_floor.py` for what goes wrong when it is not — briefly:
  # the floor is inherited from the runner image unless something pins it, and it moved
  # under us once already without a single check going red.
  case "${tag}" in
  manylinux_2_17_*)
    python3 "${repo_root}/scripts/check_glibc_floor.py" "${source}" 2.17
    ;;
  esac

  python3 "${repo_root}/scripts/make_wheel.py" \
    --version "${version}" \
    --tag "${tag}" \
    --binary "${source}" \
    --binary-name "${binary}" \
    --readme "${repo_root}/README.md" \
    --license "${repo_root}/LICENSE-MIT" \
    --license "${repo_root}/LICENSE-APACHE" \
    --out "${out}"
  built=$((built + 1))
done

echo "wheels: ${built} built in ${out}"
