#!/usr/bin/env bash
# Checks that repository scripts run where CI runs them, not just where they were written.
#
# These scripts run on three platforms and the differences do not announce themselves. A
# developer on Linux, or on a Mac with a newer bash from Homebrew, sees everything pass and
# ships something that fails only on a runner. Both of the following cost a release, in the
# same run, for unrelated reasons:
#
#   * macOS ships **bash 3.2**, from 2007, because bash 4 changed license. `mapfile` does not
#     exist there, so the publication order came back empty and every crates test failed.
#
#   * **Windows translates newlines.** Python's text-mode stdout writes CRLF, so a value read
#     from a Python helper carries a trailing carriage return. Such values still compare equal
#     to each other, which is what makes it so quiet — only a comparison against a literal
#     written in the script fails, and then everything downstream of it silently does nothing.
#
# Both are reproduced here, on whatever platform this runs on.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
self="$(basename "${BASH_SOURCE[0]}")"

passed=0
failed=0

report() {
  local name="$1" offenders="$2"
  if [ -n "${offenders}" ]; then
    failed=$((failed + 1))
    echo "FAIL ${name}"
    while IFS= read -r line; do
      [ -n "${line}" ] && echo "  ${line}"
    done <<<"${offenders}"
  else
    passed=$((passed + 1))
  fi
}

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# --- nothing depends on a bash newer than macOS ships -------------------------------------------
#
# Each of these is bash 4+. The replacement for every one is short, and the failure without it
# is a script that quietly does nothing on the one platform nobody tests on locally.
#
#   mapfile / readarray   ->  while IFS= read -r line; do ...; done < <(...)
#   declare -A            ->  parallel arrays, or a case statement
#   ${var^^} / ${var,,}   ->  tr '[:lower:]' '[:upper:]'
#
# This file is skipped, because naming a construct in order to forbid it is not using it.
report "no script uses a bash 4 construct" "$(
  for script in "${repo_root}"/scripts/*.sh; do
    [ "$(basename "${script}")" = "${self}" ] && continue
    while IFS= read -r hit; do
      [ -n "${hit}" ] && echo "$(basename "${script}"):${hit}"
    done < <(grep -nE '(^|[^[:alnum:]_-])(mapfile|readarray)[[:space:]]|declare[[:space:]]+-A|\$\{[A-Za-z_][A-Za-z0-9_]*\^\^|\$\{[A-Za-z_][A-Za-z0-9_]*,,' "${script}" |
      grep -vE '^[[:space:]]*[0-9]+:[[:space:]]*#' || true)
  done
)"

# --- the scripts survive Windows line endings ------------------------------------------------------
#
# Reproduced rather than reasoned about: a `python3` that appends a carriage return to every
# line, exactly as Python's text-mode stdout does on Windows, placed ahead of the real one on
# PATH. If the publish scripts still agree with the literals written in their tests, they are
# robust to it; if they are not, this fails here instead of twenty minutes into CI on the one
# platform that shows it.
if command -v python3 >/dev/null 2>&1; then
  real_python3="$(command -v python3)"
  mkdir -p "${work}/crlf"
  cat >"${work}/crlf/python3" <<STUB
#!/usr/bin/env bash
# \`pipefail\`, or this stub reports the exit status of \`sed\` — which is always zero — and
# every python3 invocation appears to succeed no matter what it did. That made the simulation
# silently weaker than it looked: a script whose control flow turns on python's exit code,
# like the glibc floor check, could fail every one of its cases here and still be reported
# as tolerating CRLF.
set -o pipefail
# Every line gains a trailing CR, which is what Python does on Windows.
"${real_python3}" "\$@" | sed 's/\$/\r/'
STUB
  chmod +x "${work}/crlf/python3"

  report "the publish scripts tolerate CRLF from python" "$(
    PATH="${work}/crlf:${PATH}"
    export PATH
    for suite in test-publish-npm.sh test-publish-crates.sh test-publish-pypi.sh \
      test-build-python-wheels.sh; do
      "${repo_root}/scripts/${suite}" >"${work}/${suite}.log" 2>&1 ||
        echo "${suite} fails when python3 emits CRLF: $(grep -c '^FAIL' "${work}/${suite}.log" | tr -d ' ') assertion(s)"
    done
  )"
  # --- and Windows' stdout *encoding*, which is a different failure ---------------------------
  #
  # Python's stdout on Windows is cp1252, not UTF-8. Any text carrying a character it cannot
  # represent — an em dash, of which this repository's prose has thousands — dies with
  # UnicodeEncodeError partway through, so the output is truncated at the first one rather than
  # mangled. That reads as "the assertion is wrong" rather than "the write failed".
  #
  # Simulated with PYTHONIOENCODING rather than a stub, because it is the same switch Windows
  # flips. A helper reading a wheel's METADATA — which embeds the README — passed on Linux and
  # macOS and failed four assertions on Windows, which cost a CI round trip to learn.
  report "the test suites tolerate a cp1252 stdout" "$(
    for suite in test-publish-npm.sh test-publish-crates.sh test-publish-pypi.sh \
      test-build-python-wheels.sh; do
      PYTHONIOENCODING=cp1252 "${repo_root}/scripts/${suite}" >"${work}/${suite}.cp1252.log" 2>&1 ||
        echo "${suite} fails when stdout cannot encode UTF-8: $(grep -c '^FAIL' "${work}/${suite}.cp1252.log" | tr -d ' ') assertion(s)"
    done
  )"
else
  echo "note: no python3 here, so the CRLF simulation is skipped"
fi

# --- the scripts actually run under bash 3.2 -----------------------------------------------------
#
# The static check above catches what it knows to look for; running them catches the rest. Only
# possible where a 3.x bash exists — every Mac has one at /bin/bash — so this is a real
# assertion there and honestly skipped elsewhere rather than faked.
if [ -x /bin/bash ] && /bin/bash --version 2>/dev/null | head -1 | grep -q 'version 3\.'; then
  # Parsing first, and every script rather than the two that get run. It costs nothing and it
  # catches a whole class the static grep above cannot see — bash 3.2 parses `$(...)` without
  # understanding a heredoc inside it, so a single apostrophe in an embedded Python comment
  # ("ZipInfo's") reads as an opening quote and the file fails to parse at all. Newer bash is
  # perfectly happy with it, so nothing short of this notices.
  report "every script parses under bash 3.2" "$(
    for script in "${repo_root}"/scripts/*.sh; do
      /bin/bash -n "${script}" 2>&1 | head -1 |
        sed "s|^|$(basename "${script}"): |;s|^\(.*\): $|\1|" | grep -v ': *$' || true
    done
  )"

  report "the publish scripts' tests pass under bash 3.2" "$(
    for suite in test-publish-npm.sh test-publish-crates.sh test-publish-pypi.sh \
      test-build-python-wheels.sh; do
      /bin/bash "${repo_root}/scripts/${suite}" >/dev/null 2>&1 ||
        echo "${suite} fails under $(/bin/bash --version | head -1)"
    done
  )"
else
  echo "note: no bash 3.x here, so the bash 3.2 run is skipped (macOS CI covers it)"
fi

echo
echo "${passed} passed, ${failed} failed"
[ "${failed}" -eq 0 ]
