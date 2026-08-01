#!/usr/bin/env bash
# Assemble the downloadable archives a GitHub release attaches, from the binaries a release
# build produced.
#
# Run from the repository root:
#
#   ./scripts/build-release-archives.sh 0.1.1 dist [output-dir]
#
# where `dist/` holds one directory per target triple, each containing the built binary —
# the layout the release workflow's download-artifact step produces. This is the same input
# `build-npm-packages.sh` takes, deliberately: one build feeds both distribution channels, so
# the binary on the releases page and the binary npm installs are the same bytes.
#
# Output is one archive per platform plus `SHA256SUMS`. Windows gets a zip because that is
# what unpacks without extra tooling there; everything else gets a tarball.
#
# Each archive carries both licenses and the README alongside the binary. Someone who
# downloads a tarball rather than installing from a registry has no other copy of either.
set -euo pipefail

version="${1:?usage: build-release-archives.sh <version> <artifacts-dir> [output-dir]}"
artifacts="${2:?usage: build-release-archives.sh <version> <artifacts-dir> [output-dir]}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${3:-${repo_root}/dist-archives}"

rm -rf "${out}"
mkdir -p "${out}"

# target triple : binary name
targets=(
  "aarch64-apple-darwin:lanekeep"
  "aarch64-unknown-linux-gnu:lanekeep"
  "x86_64-unknown-linux-gnu:lanekeep"
  "x86_64-pc-windows-msvc:lanekeep.exe"
)

staging="${out}/.staging"
built=0

for entry in "${targets[@]}"; do
  triple="${entry%%:*}"
  binary="${entry##*:}"
  source="${artifacts}/${triple}/${binary}"

  if [ ! -f "${source}" ]; then
    # Loud, not silent. A platform missing from the releases page is a platform whose users
    # are told to build from source, and nothing else would say so until one of them asked.
    echo "error: no binary for ${triple} at ${source}" >&2
    exit 1
  fi

  name="lanekeep-${version}-${triple}"
  rm -rf "${staging}"
  mkdir -p "${staging}/${name}"

  cp "${source}" "${staging}/${name}/${binary}"
  # Restored here for the same reason the npm publish restores it: `upload-artifact` zips its
  # input and `download-artifact` unpacks it without permissions, so what arrives is 0644 and
  # a tarball made from it ships a binary nobody can execute.
  chmod +x "${staging}/${name}/${binary}"
  cp "${repo_root}/README.md" "${repo_root}/LICENSE-MIT" "${repo_root}/LICENSE-APACHE" \
    "${staging}/${name}/"

  case "${triple}" in
  *windows*)
    # Built with Python rather than `zip`, which git-bash does not ship — the script has to
    # run on the same three platforms everything else here does, and `zip` is missing on the
    # one that needs the zip.
    #
    # Entries are sorted and carry no timestamp of their own, so the same input produces the
    # same bytes. Permissions are written explicitly because a zip records them separately
    # from the filesystem, and the binary has to come out executable.
    python3 - "${staging}" "${name}" "${out}/${name}.zip" <<'PY'
import os
import stat
import sys
import zipfile

staging, name, destination = sys.argv[1], sys.argv[2], sys.argv[3]
root = os.path.join(staging, name)

with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED) as archive:
    for directory, _, filenames in os.walk(root):
        for filename in sorted(filenames):
            path = os.path.join(directory, filename)
            relative = os.path.relpath(path, staging).replace(os.sep, "/")

            # ZipInfo's default timestamp is fixed, which is what keeps this reproducible.
            entry = zipfile.ZipInfo(relative)
            entry.compress_type = zipfile.ZIP_DEFLATED
            mode = os.stat(path).st_mode
            # An executable stays executable; everything else is 0644.
            permissions = 0o755 if mode & stat.S_IXUSR else 0o644
            entry.external_attr = permissions << 16

            with open(path, "rb") as handle:
                archive.writestr(entry, handle.read())
PY
    echo "packaged ${name}.zip"
    ;;
  *)
    tar -czf "${out}/${name}.tar.gz" -C "${staging}" "${name}"
    echo "packaged ${name}.tar.gz"
    ;;
  esac

  built=$((built + 1))
done

rm -rf "${staging}"

# Checksums, so someone downloading a binary can tell whether they got the one that was
# built. Written with plain names rather than paths, which is what `sha256sum -c` expects to
# find beside it.
(
  cd "${out}"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum ./*.tar.gz ./*.zip | sed 's|\./||' >SHA256SUMS
  else
    # macOS has shasum rather than sha256sum, and its output already omits the ./ prefix.
    shasum -a 256 ./*.tar.gz ./*.zip | sed 's|\./||' >SHA256SUMS
  fi
)

echo "packaged ${built} archive(s) and SHA256SUMS at ${version}"
