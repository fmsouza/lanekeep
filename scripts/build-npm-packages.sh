#!/usr/bin/env bash
# Assemble the npm platform packages from binaries a release build produced.
#
# One package per platform plus the launcher, published together. npm installs only the
# platform package matching the machine, because each declares `os` and `cpu` and the
# launcher lists them all as optional dependencies.
#
# Run from the repository root:
#
#   ./scripts/build-npm-packages.sh 0.1.0 dist/ [output-dir]
#
# where `dist/` holds one directory per target triple, each containing the built binary.
# That is the layout the release workflow's download-artifact step produces.
#
# Output goes to a directory of its own — `dist-npm/` by default — and never back into
# `npm/`. `npm/lanekeep/package.json` is a *template*: its version is `0.0.0` in the
# repository and is only ever rewritten in a copy. Rewriting it in place would leave a
# release version committed, which is both a confusing diff and a thing someone would
# eventually publish by accident.
set -euo pipefail

version="${1:?usage: build-npm-packages.sh <version> <artifacts-dir> [output-dir]}"
artifacts="${2:?usage: build-npm-packages.sh <version> <artifacts-dir> [output-dir]}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${3:-${repo_root}/dist-npm}"

rm -rf "${out}"
mkdir -p "${out}"
cp -R "${repo_root}/npm/lanekeep" "${out}/lanekeep"

# target triple : npm platform : npm cpu : binary name
targets=(
  "aarch64-apple-darwin:darwin:arm64:lanekeep"
  "aarch64-unknown-linux-gnu:linux:arm64:lanekeep"
  "x86_64-unknown-linux-gnu:linux:x64:lanekeep"
  "x86_64-pc-windows-msvc:win32:x64:lanekeep.exe"
)

built=0
for entry in "${targets[@]}"; do
  IFS=: read -r triple os cpu binary <<<"${entry}"
  name="${os}-${cpu}"
  source="${artifacts}/${triple}/${binary}"

  if [ ! -f "${source}" ]; then
    # Loud, not silent. A platform package missing from a release is a platform whose users
    # get "lanekeep does not ship a binary for your machine" — and nothing else would say so
    # until one of them reported it.
    echo "error: no binary for ${triple} at ${source}" >&2
    exit 1
  fi

  package="${out}/@lanekeep/${name}"
  mkdir -p "${package}/bin"
  cp "${source}" "${package}/bin/${binary}"
  chmod +x "${package}/bin/${binary}"

  cat >"${package}/package.json" <<JSON
{
  "name": "@lanekeep/${name}",
  "version": "${version}",
  "description": "lanekeep binary for ${os} ${cpu}",
  "license": "MIT OR Apache-2.0",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/fmsouza/lanekeep.git"
  },
  "os": ["${os}"],
  "cpu": ["${cpu}"],
  "files": ["bin/${binary}"]
}
JSON

  echo "packaged @lanekeep/${name}"
  built=$((built + 1))
done

# The launcher's own version, and the versions it depends on, are written from the tag — into
# the copy, never into the template. A committed version is one more thing to forget on a
# release, and forgetting it publishes a launcher that pulls last release's binaries.
launcher="${out}/lanekeep/package.json"
python3 - "${launcher}" "${version}" <<'PY'
import json
import sys

path, version = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    package = json.load(handle)

package["version"] = version
package["optionalDependencies"] = {
    name: version for name in package.get("optionalDependencies", {})
}

with open(path, "w", encoding="utf-8") as handle:
    json.dump(package, handle, indent=2)
    handle.write("\n")
PY

# Every platform the launcher can resolve must have been built, or an install succeeds and
# then fails at first run on that platform.
python3 - "${launcher}" "${out}" <<'PY'
import json
import os
import sys

launcher, out = sys.argv[1], sys.argv[2]
with open(launcher, encoding="utf-8") as handle:
    declared = set(json.load(handle)["optionalDependencies"])

missing = [
    name
    for name in declared
    if not os.path.isdir(os.path.join(out, name))
]
if missing:
    print(f"error: launcher declares {sorted(missing)} but they were not built", file=sys.stderr)
    raise SystemExit(1)
PY

# The authoring package's types travel with the launcher rather than as a package of their
# own. `lanekeep` is the name a rule imports from — `import { defineRule } from 'lanekeep'` —
# so the types have to live under that name for an editor to find them, and a second package
# would be a second thing to install for no gain.
#
# Nothing here runs in Node: lanekeep evaluates rules in its own sandbox, where `lanekeep`
# resolves to a host module. `index.js` exists so a tool that *does* load a rule under Node
# finds something coherent, and `index.d.ts` is what gives an author autocomplete.
for types in index.js index.d.ts builtin.d.ts; do
  cp "${repo_root}/packages/lanekeep/${types}" "${out}/lanekeep/${types}"
done

cp "${repo_root}/README.md" "${out}/lanekeep/README.md"
echo "packaged lanekeep (launcher) with ${built} platform package(s) at ${version}"
