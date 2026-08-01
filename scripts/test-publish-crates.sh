#!/usr/bin/env bash
# Tests for publish-crates.sh.
#
# crates.io cannot take a version back, so the sequence this script drives is the least
# recoverable thing in the release. The property worth pinning is that a run which died
# partway can be re-run to completion at the same version, rather than requiring a fresh
# version number every time something goes wrong.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/publish-crates.sh"

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

cat >"${work}/cargo" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${CARGO_LOG}"
exit 0
STUB
chmod +x "${work}/cargo"

# Stands in for the crates.io lookup, so the tests never touch the network.
cat >"${work}/probe" <<'STUB'
#!/usr/bin/env bash
grep -qxF "$1" "${CRATES_PUBLISHED}" 2>/dev/null && exit 0
exit 1
STUB
chmod +x "${work}/probe"

export CARGO="${work}/cargo"
export CRATES_PROBE="${work}/probe"
export CARGO_LOG="${work}/log"
export CRATES_PUBLISHED="${work}/published"

reset() {
  : >"${CARGO_LOG}"
  : >"${CRATES_PUBLISHED}"
}

# The crates handed to `cargo publish -p`, in order.
publish_order() {
  grep '^publish -p ' "${CARGO_LOG}" | awk '{print $3}'
}

# --- a clean registry publishes the whole workspace -----------------------------------------
reset
"${script}" >/dev/null 2>&1
check "a clean run succeeds" "0" "$?"
check "every crate is published" "12" "$(publish_order | wc -l | tr -d ' ')"

# --- dependency order ------------------------------------------------------------------------
#
# crates.io rejects a crate whose dependencies are not yet on it, so this order is load
# bearing. The binary must be last and the foundation first.
check "the core crate goes first" "lanekeep-core" "$(publish_order | head -1)"
check "the binary goes last" "lanekeep-cli" "$(publish_order | tail -1)"

# A few edges that would break the run if they inverted.
order="$(publish_order | tr '\n' ' ')"
before() {
  local first="$1" second="$2" a b
  a="$(publish_order | grep -nxF "${first}" | cut -d: -f1)"
  b="$(publish_order | grep -nxF "${second}" | cut -d: -f1)"
  [ -n "${a}" ] && [ -n "${b}" ] && [ "${a}" -lt "${b}" ] && echo yes || echo no
}
check "the engine follows the cache" "yes" "$(before lanekeep-cache lanekeep-engine)"
check "the engine follows the sandbox" "yes" "$(before lanekeep-js lanekeep-engine)"
check "the rules follow the engine" "yes" "$(before lanekeep-engine lanekeep-rules)"
check "the language registry precedes its grammars" "yes" \
  "$(before lanekeep-lang lanekeep-lang-js)"

# --- a run that died partway resumes -----------------------------------------------------------
#
# The whole reason the skip exists. Six crates already up, six to go: a re-run must publish
# the remaining six and not stop on the first one it cannot republish.
reset
version="$(python3 -c '
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
section = text.split("[workspace.package]", 1)[1]
print(re.search(r"^version\s*=\s*\"([^\"]+)\"", section, re.MULTILINE).group(1))
' "${repo_root}/Cargo.toml")"

for crate in lanekeep-core lanekeep-lang lanekeep-lang-js lanekeep-query lanekeep-js \
  lanekeep-cache; do
  echo "${crate}" >>"${CRATES_PUBLISHED}"
done

"${script}" >/dev/null 2>&1
check "a partial release resumes" "0" "$?"
check "and publishes only the remainder" "6" "$(publish_order | wc -l | tr -d ' ')"
check "starting where it stopped" "lanekeep-config" "$(publish_order | head -1)"

# --- a complete release is a no-op ---------------------------------------------------------------
reset
for crate in lanekeep-core lanekeep-lang lanekeep-lang-js lanekeep-query lanekeep-js \
  lanekeep-cache lanekeep-config lanekeep-engine lanekeep-report lanekeep-rules \
  lanekeep-testkit lanekeep-cli; do
  echo "${crate}" >>"${CRATES_PUBLISHED}"
done
"${script}" >/dev/null 2>&1
check "a fully published release succeeds" "0" "$?"
check "and publishes nothing again" "0" "$(publish_order | wc -l | tr -d ' ')"

# --- the version comes from the workspace ----------------------------------------------------------
reset
check "the workspace version is what it reports" "1" \
  "$("${script}" 2>/dev/null | grep -cF "lanekeep ${version} to crates.io" | tr -d ' ')"

# --- verification is never skipped ---------------------------------------------------------------
#
# A crate that cannot build from its own published form is a crate nobody can use, and
# `--no-verify` would hide exactly that.
reset
"${script}" >/dev/null 2>&1
check "publishing never passes --no-verify" "0" \
  "$(grep -c -- '--no-verify' "${CARGO_LOG}" | tr -d ' ')"

# --- flags ------------------------------------------------------------------------------------------
reset
"${script}" --dry-run >/dev/null 2>&1
check "--dry-run is forwarded" "12" "$(grep -c -- '--dry-run' "${CARGO_LOG}" | tr -d ' ')"

reset
"${script}" --nonsense >/dev/null 2>&1
check "an unknown flag is refused" "2" "$?"

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
