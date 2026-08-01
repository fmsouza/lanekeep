#!/usr/bin/env bash
# Publish the assembled npm packages, skipping any version already on the registry.
#
# Run from the repository root, against what `build-npm-packages.sh` produced:
#
#   ./scripts/publish-npm.sh dist-npm
#
# Two properties this exists to hold, both of which v0.1.0's first release broke.
#
# **Every path is passed with a leading `./`.** `npm publish dist-npm/lanekeep` does not
# publish a directory: npm reads a bare `<a>/<b>` as a GitHub shorthand and tries to clone
# `ssh://git@github.com/dist-npm/lanekeep.git`, which fails with a public-key error naming
# a repository nobody has heard of. The platform packages escaped this only by accident,
# because a glob ending in `/` left a trailing slash on each path and a trailing slash is
# enough to make npm see a directory.
#
# **A version already on the registry is skipped, not retried.** npm refuses to republish a
# version, so without this a release that fails partway can never be resumed: the re-run
# dies on the first already-published package and never reaches the one that failed. That
# is exactly how v0.1.0 got stuck with four platform packages up and no launcher.
set -euo pipefail

dist="${1:?usage: publish-npm.sh <dist-dir> [--dry-run]}"
shift

extra=()
for argument in "$@"; do
  case "${argument}" in
  --dry-run) extra+=(--dry-run) ;;
  *)
    echo "error: unknown argument ${argument}" >&2
    exit 2
    ;;
  esac
done

# Overridable so the tests can drive a stub. Nothing else sets it.
npm_command="${NPM:-npm}"

# Make a path npm cannot mistake for `<org>/<repo>`. The whole point of this script.
as_path() {
  case "$1" in
  /* | ./* | ../*) printf '%s' "$1" ;;
  *) printf './%s' "$1" ;;
  esac
}

field() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2"
}

publish() {
  local directory="${1%/}" name version
  name="$(field "${directory}/package.json" name)"
  version="$(field "${directory}/package.json" version)"

  # `npm view` exits non-zero when the version is unknown, which is the answer we want and
  # not an error. Anything it writes is noise either way.
  if "${npm_command}" view "${name}@${version}" version >/dev/null 2>&1; then
    echo "skip    ${name}@${version} (already published)"
    return 0
  fi

  # Restore the executable bit immediately before packing.
  #
  # `build-npm-packages.sh` sets it, and it does not survive the trip between jobs:
  # `actions/upload-artifact` zips its input and `download-artifact` unpacks it without
  # permissions, so what arrives here is 0644. npm packs whatever mode it finds — it does not
  # normalize, and it does not infer one from `bin` — so the published tarball ships a binary
  # nobody can execute, and every `lanekeep` invocation dies with `EACCES`.
  #
  # v0.1.0 shipped exactly that. Nothing catches it before someone installs the published
  # package: the artifact is fine when built, fine when packaged, and wrong only after a
  # round trip that happens between two jobs neither script can see.
  local binary
  for binary in "${directory}"/bin/*; do
    [ -f "${binary}" ] && chmod +x "${binary}"
  done

  echo "publish ${name}@${version}"
  "${npm_command}" publish "$(as_path "${directory}")" \
    --access public --provenance ${extra[@]+"${extra[@]}"}
}

# The two registries learn the version from different places: npm from the tag, by way of the
# packaging script, and crates.io from the workspace manifest. If those ever disagree, one
# release ships under two different numbers, and neither registry lets a number be reused. It
# costs nothing to notice here, before anything is published.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workspace_version="$(python3 - "${repo_root}/Cargo.toml" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
section = text.split("[workspace.package]", 1)[1]
print(re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE).group(1))
PY
)"
launcher_version="$(field "${dist}/lanekeep/package.json" version)"
if [ "${launcher_version}" != "${workspace_version}" ]; then
  echo "error: npm would publish ${launcher_version}, but Cargo.toml says ${workspace_version}" >&2
  echo "       the tag and the workspace manifest have to agree before either registry sees it" >&2
  exit 1
fi

# Platform packages first. The launcher lists them as optional dependencies, so publishing
# it first opens a window in which `npm install lanekeep` resolves a launcher whose binaries
# do not exist yet.
platforms=0
for package in "${dist}"/@lanekeep/*/; do
  [ -d "${package}" ] || continue
  publish "${package}"
  platforms=$((platforms + 1))
done

# A launcher with no platform packages beside it installs and then fails at first run, on
# every machine. Better to refuse than to publish that.
if [ "${platforms}" -eq 0 ]; then
  echo "error: no platform packages found under ${dist}/@lanekeep/" >&2
  exit 1
fi

publish "${dist}/lanekeep"
echo "npm: ${platforms} platform package(s) and the launcher are at the published version"
