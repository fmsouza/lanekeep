#!/usr/bin/env bash
# Checks that release-plz and release.yml agree on what a release is called.
#
# They are two halves of one process that share no state but the tag name, and nothing makes
# them agree. release-plz tags and creates the release; `release.yml` fires on the tag and
# attaches the binaries. Get the name wrong and both halves succeed separately: a release with
# no binaries on it, and binaries attached to a tag with no release. That is what v0.1.1 did.
#
# A config file cannot fail a test on its own, so these assert the shape it has to keep.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config="${repo_root}/release-plz.toml"
workflow="${repo_root}/.github/workflows/release.yml"

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

if ! command -v python3 >/dev/null 2>&1; then
  # Said out loud rather than passing silently, like every other check here.
  echo "note: python3 is unavailable — release config checks skipped"
  exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# Python output goes through a file, never `$(...)`: bash 3.2 cannot parse a heredoc inside
# command substitution when it contains an apostrophe.
python3 - "${config}" >"${work}/report" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)

workspace = config.get("workspace", {})
packages = config.get("package", [])

# Whether a package tags or releases, resolving the workspace default it inherits.
def resolves_to(package, field):
    return package.get(field, workspace.get(field, False))

tagging = [p["name"] for p in packages if resolves_to(p, "git_tag_enable")]
releasing = [p["name"] for p in packages if resolves_to(p, "git_release_enable")]

print(f"workspace_tag_default={workspace.get('git_tag_enable')}")
print(f"workspace_release_default={workspace.get('git_release_enable')}")
print(f"tag_name={workspace.get('git_tag_name')}")
print(f"release_name={workspace.get('git_release_name')}")
print(f"tagging={','.join(sorted(tagging))}")
print(f"releasing={','.join(sorted(releasing))}")

writing = [p["name"] for p in packages if resolves_to(p, "changelog_update")]
print(f"workspace_changelog_default={workspace.get('changelog_update')}")
print(f"writing={','.join(sorted(writing))}")
for package in packages:
    if resolves_to(package, "changelog_update"):
        print(f"changelog_path={package.get('changelog_path')}")
PY

value() { grep "^$1=" "${work}/report" | cut -d= -f2-; }

# Exactly one crate tags, and exactly one releases. Two would mean two tags per release and
# the duplicates are back; zero would mean release.yml is never triggered at all.
check "exactly one crate is tagged" "lanekeep-cli" "$(value tagging)"
check "exactly one crate is released" "lanekeep-cli" "$(value releasing)"

# The defaults have to be off, or a crate added later inherits tagging without anyone saying
# so — which is precisely how twelve tags appeared.
check "tagging is off by default" "False" "$(value workspace_tag_default)"
check "releasing is off by default" "False" "$(value workspace_release_default)"

# The names release.yml depends on. Its trigger is `v*` and it attaches to `github.ref_name`,
# so a tag not spelled this way is a tag it never sees.
check "the tag is named for the version alone" "v{{ version }}" "$(value tag_name)"
check "the release is named for the version alone" "v{{ version }}" "$(value release_name)"

# --- one changelog, at the root, covering everything ----------------------------------------
#
# Per-crate changelogs feed a loop: release-plz writes them into `crates/*/`, then counts a
# changed file under a crate as a change to that crate and proposes a release for it. v0.2.0
# shipped and the very next run proposed 0.2.1 with no code change at all.
check "changelogs are off by default" "False" "$(value workspace_changelog_default)"
check "exactly one crate writes a changelog" "lanekeep-cli" "$(value writing)"
check "and writes it to the repository root" "CHANGELOG.md" "$(value changelog_path)"

# `changelog_include` has to name every *other* workspace crate, compared against what cargo
# reports rather than against a copy written down here. A crate added later and forgotten
# would vanish from the changelog and from the GitHub release body, which is generated from
# it — a change that appears in neither, with nothing to say so.
python3 - "${config}" >"${work}/coverage" <<'PY'
import json
import subprocess
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)

workspace = config.get("workspace", {})
writers = [
    package
    for package in config.get("package", [])
    if package.get("changelog_update", workspace.get("changelog_update", False))
]
if len(writers) != 1:
    print(f"expected exactly one changelog writer, found {len(writers)}")
    raise SystemExit(0)

writer = writers[0]
included = set(writer.get("changelog_include", []))

metadata = subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--no-deps"],
    capture_output=True,
    text=True,
    check=True,
)
members = {
    package["name"]
    for package in json.loads(metadata.stdout)["packages"]
    if package.get("publish") != []
}

problems = []
missing = sorted((members - {writer["name"]}) - included)
if missing:
    problems.append(f"not covered by the changelog: {', '.join(missing)}")
unknown = sorted(included - members)
if unknown:
    problems.append(f"named but not in the workspace: {', '.join(unknown)}")

print("; ".join(problems))
PY
check "the changelog covers every other crate" "" "$(tr -d '\r' <"${work}/coverage")"

# And the other half: release.yml has to actually fire on that name. Both YAML spellings —
# `tags: ["v*"]` and a block list — because which one is used is not the point.
check "release.yml triggers on the tag release-plz creates" "0" \
  "$(grep -qE "tags:.*v\*|^[[:space:]]*-[[:space:]]*['\"]?v\*" "${workflow}" && echo 0 || echo 1)"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
