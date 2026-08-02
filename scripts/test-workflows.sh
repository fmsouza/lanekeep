#!/usr/bin/env bash
# Checks on the workflow files that YAML validity does not cover.
#
# Each one here is a mistake that produces a *working* workflow doing the wrong thing
# quietly. Nothing else catches them: the file parses, the run is green, and the behavior is
# wrong in a way only a careful reading reveals.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflows="${repo_root}/.github/workflows"

passed=0
failed=0

# Python output goes through a file rather than `$(...)`.
#
# bash 3.2 — macOS's — parses command substitution without understanding a heredoc inside it,
# so an apostrophe in an embedded Python comment reads as an opening quote and the whole file
# fails to parse. Nothing here caught that: this script exits early where pyyaml is missing,
# which is the case on the macOS runner, so it never reached the offending line.
findings="$(mktemp)"
trap 'rm -f "${findings}"' EXIT

report() {
  local name="$1" offenders="$2"
  if [ -n "${offenders}" ]; then
    failed=$((failed + 1))
    echo "FAIL ${name}"
    # Piped rather than word-split, so a finding containing spaces stays one line.
    while IFS= read -r line; do
      [ -n "${line}" ] && echo "  ${line}"
    done <<<"${offenders}"
  else
    passed=$((passed + 1))
  fi
}

if ! command -v python3 >/dev/null 2>&1 || ! python3 -c "import yaml" 2>/dev/null; then
  # Said out loud rather than passing silently. A check that quietly does nothing is the
  # thing these checks exist to prevent.
  echo "note: python3 with pyyaml is unavailable — workflow checks skipped"
  exit 0
fi

# --- a step must not gate on a variable it sets itself ----------------------------------
#
# A step's own `env:` does not exist when its `if:` is evaluated — the condition decides
# whether the step runs at all. So `if: env.TOKEN != ''` on a step that sets `TOKEN` in its
# own `env:` reads an undefined variable and is always false.
#
# In a publish step that is the worst available failure: the release reports success and
# publishes nothing, indistinguishable from a deliberate dry run. Decide in one step and
# branch on its output instead.
python3 - "${workflows}" >"${findings}" <<'PY'
import pathlib
import re
import sys

import yaml

found = []
for path in sorted(pathlib.Path(sys.argv[1]).glob("*.yml")):
    document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    for job_name, job in (document.get("jobs") or {}).items():
        for step in job.get("steps") or []:
            condition = step.get("if")
            if not isinstance(condition, str):
                continue
            own = set((step.get("env") or {}).keys())
            for name in re.findall(r"env\.([A-Za-z_][A-Za-z0-9_]*)", condition):
                if name in own:
                    found.append(
                        f"{path.name} :: {job_name} :: {step.get('name', '(unnamed)')} "
                        f"gates on env.{name}, which it sets itself"
                    )

print("\n".join(found))
PY
report "no step gates on a variable it sets itself" "$(cat "${findings}")"

# --- every action is pinned to a commit SHA ----------------------------------------------
#
# A tag is mutable: `uses: some/action@v4` runs whatever that tag points at today, which is
# a supply-chain hole in a workflow holding registry tokens. A pin is forty hex characters —
# checked for what it *is* rather than for what a tag looks like, because a SHA beginning
# with a digit is indistinguishable from a version otherwise. (This check's first draft got
# that backwards and flagged every correctly pinned action.)
python3 - "${workflows}" >"${findings}" <<'PY'
import pathlib
import re
import sys

import yaml

PINNED = re.compile(r"^[^@]+@[0-9a-f]{40}$")

found = []
for path in sorted(pathlib.Path(sys.argv[1]).glob("*.yml")):
    document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    for job_name, job in (document.get("jobs") or {}).items():
        for step in job.get("steps") or []:
            uses = step.get("uses")
            # A local action — `./.github/actions/x` — has no upstream to pin.
            if not isinstance(uses, str) or uses.startswith("./"):
                continue
            if not PINNED.match(uses):
                found.append(f"{path.name} :: {job_name} :: {uses}")

print("\n".join(found))
PY
report "every action is pinned to a commit SHA" "$(cat "${findings}")"

# --- no run: block interpolates anything an outsider controls ------------------------------
#
# `${{ }}` is substituted before bash sees the script, so text an outsider supplies — a pull
# request title, a branch name, a tag — becomes shell source. Pass it through `env:` instead.
#
# Only the contexts an outsider can actually influence. `${{ matrix.target }}` is written in
# this repository and interpolating it is fine; flagging it, as this check's first draft did,
# would make the check something to ignore rather than something to read.
python3 - "${workflows}" >"${findings}" <<'PY'
import pathlib
import re
import sys

import yaml

# github.event.* covers titles, bodies, branch names on a fork's pull request.
UNTRUSTED = re.compile(
    r"\$\{\{\s*(github\.event\b|github\.head_ref\b|github\.ref_name\b|inputs\.)"
)

found = []
for path in sorted(pathlib.Path(sys.argv[1]).glob("*.yml")):
    document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    for job_name, job in (document.get("jobs") or {}).items():
        for step in job.get("steps") or []:
            script = step.get("run")
            if isinstance(script, str) and UNTRUSTED.search(script):
                found.append(
                    f"{path.name} :: {job_name} :: {step.get('name', '(unnamed)')}"
                )

print("\n".join(found))
PY
report "no run: block interpolates outsider-controlled text" "$(cat "${findings}")"

# --- a job publishing with provenance must be able to mint an OIDC token ------------------
#
# `npm publish --provenance` asks npm to sign an attestation linking the tarball to this
# workflow run, and it needs `id-token: write` to get the token that proves the run's
# identity. Without it the publish fails outright.
#
# That failure is invisible until the first real release: the step is skipped whenever the
# registry secrets are absent, so no dry run ever reaches it.
python3 - "${workflows}" >"${findings}" <<'PY'
import pathlib
import sys

import yaml

found = []
for path in sorted(pathlib.Path(sys.argv[1]).glob("*.yml")):
    document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    for job_name, job in (document.get("jobs") or {}).items():
        publishes = any(
            "--provenance" in str(step.get("run") or "")
            for step in job.get("steps") or []
        )
        if not publishes:
            continue

        permissions = job.get("permissions")
        granted = permissions.get("id-token") if isinstance(permissions, dict) else None
        if granted != "write":
            found.append(
                f"{path.name} :: {job_name} publishes with --provenance "
                f"but does not grant id-token: write"
            )

print("\n".join(found))
PY
report "provenance publishing has id-token: write" "$(cat "${findings}")"

# --- deciding whether to commit must account for untracked files ---------------------------
#
# `git diff` reports on tracked files only. A workflow that writes a file into a checkout and
# then asks `git diff --quiet` whether to commit gets "nothing to do" whenever that file is
# new — so it announces success and pushes nothing, which is the exact opposite of its job and
# looks identical to there genuinely being no change.
#
# Staging first and comparing `--cached` covers both cases. This fired on the Homebrew tap
# step, where the only run that would have hit it is the one against a tap with no formula in
# it yet — the first one.
python3 - "${workflows}" >"${findings}" <<'PY'
import pathlib
import re
import sys

import yaml

found = []
for path in sorted(pathlib.Path(sys.argv[1]).glob("*.yml")):
    document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    for job_name, job in (document.get("jobs") or {}).items():
        for step in job.get("steps") or []:
            script = step.get("run")
            if not isinstance(script, str):
                continue
            # Only where the answer decides a commit; a diff used to *show* something is fine.
            if "git commit" not in script:
                continue
            for line in script.splitlines():
                # A comment explaining the trap is not the trap. This check's first draft
                # flagged the very comment documenting the fix.
                if line.lstrip().startswith("#"):
                    continue
                if re.search(r"git diff\b", line) and "--cached" not in line and "--staged" not in line:
                    found.append(
                        f"{path.name} :: {job_name} :: {step.get('name', '(unnamed)')} "
                        f"decides on `git diff` without staging, so a new file reads as no change"
                    )

print("\n".join(found))
PY
report "committing decisions account for untracked files" "$(cat "${findings}")"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
