#!/usr/bin/env bash
#
# One-time development setup: verify the toolchain, install the tools the checks
# need, and activate the committed git hooks.
#
# Safe to re-run. Anything already present is left alone.

set -euo pipefail

cd "$(dirname "$0")/.."

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
ok()   { printf '  \033[32m✔\033[0m %s\n' "$1"; }
info() { printf '  \033[34m→\033[0m %s\n' "$1"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$1"; }
die()  { printf '  \033[31m✖\033[0m %s\n' "$1" >&2; exit 1; }

bold "lanekeep development setup"
echo

# ---------------------------------------------------------------------------
bold "toolchain"
# ---------------------------------------------------------------------------

command -v rustup >/dev/null 2>&1 \
    || die "rustup is not installed. See https://rustup.rs"

# rust-toolchain.toml pins the version; this materializes it.
info "installing the pinned toolchain from rust-toolchain.toml"
rustup show active-toolchain >/dev/null
ok "$(rustc --version)"

# Rule components are built for wasm32-unknown-unknown, whose import list is exactly the
# declared world. `cargo component`'s default target, wasm32-wasip1, imports a wall clock
# and a filesystem the moment a guest touches std, so the target is a requirement rather
# than a preference and installing it here is what makes the requirement satisfiable.
# Not needed to run any gate — the built fixtures are committed.
info "installing the wasm32-unknown-unknown target"
rustup target add wasm32-unknown-unknown
ok "wasm32-unknown-unknown"

# And the target every line above exists to avoid, because one fixture is built for it on
# purpose: tests/fixtures/rejected/wasip1.wasm is what the load-time import check is pointed at,
# and `AGENTS.md` records why it has to be built rather than described — a guest small enough to
# allocate nothing has zero imports on *both* targets, so a synthetic approximation would prove
# nothing. Installed here for the same reason as the target above: without it `just
# wasm-fixtures` gets nine fixtures in and dies on a rustup error about a target nobody asked
# for, which reads like the recipe is wrong rather than the toolchain incomplete.
info "installing the wasm32-wasip1 target"
rustup target add wasm32-wasip1
ok "wasm32-wasip1"

# ---------------------------------------------------------------------------
bold "tools"
# ---------------------------------------------------------------------------

# Binary name, then the cargo package providing it.
TOOLS=(
    "just:just"
    "cargo-nextest:cargo-nextest"
    "cargo-deny:cargo-deny"
    "cargo-machete:cargo-machete"
    "cargo-insta:cargo-insta"
    "cargo-audit:cargo-audit"
    "typos:typos-cli"
    # Builds the WebAssembly rule components. `crates/lanekeep-rules/build.rs` reaches for
    # it on a fresh checkout, where the shipped Rust components are not built yet, and
    # `just wasm-fixtures` needs it to rebuild a committed fixture.
    "cargo-component:cargo-component"
    # `tinygo build` shells out to this for `component embed` and `component new`, so
    # `just go-rules` needs it and nothing else here does. Installed rather than merely
    # required, because `_require`'s message tells you to run this script — a promise that
    # was false for wasm-tools until this line existed. TinyGo itself cannot be installed
    # from cargo; docs/authoring-go-rules.md says so and says what to do instead.
    #
    # **Pinned, and to the version `.github/workflows/ci.yml`'s `go-rules` job downloads.**
    # That job rebuilds the committed Go artifacts and fails on a byte diff, and this is the
    # encoder that writes their component sections — so an unpinned install here is a
    # contributor building through one encoder and CI diffing against another, which reddens
    # a job about the rule they changed for a reason that is not about it. The version lives
    # in two places and they have to move together; the justfile's `go-rules` recipe echoes
    # what it actually used, which is where a mismatch is readable.
    #
    # The pin reaches a fresh machine and not an existing one: the loop below skips anything
    # already on PATH, whatever version it is.
    "wasm-tools:wasm-tools@1.255.0"
)

missing=()
for entry in "${TOOLS[@]}"; do
    bin="${entry%%:*}"
    pkg="${entry##*:}"
    if command -v "$bin" >/dev/null 2>&1; then
        ok "$bin"
    else
        missing+=("$pkg")
        info "$bin is missing, will install $pkg"
    fi
done

if [ ${#missing[@]} -gt 0 ]; then
    echo
    if command -v cargo-binstall >/dev/null 2>&1; then
        info "installing ${#missing[@]} tool(s) with cargo-binstall"
        cargo binstall --no-confirm --locked "${missing[@]}"
    else
        warn "cargo-binstall not found; falling back to 'cargo install', which compiles from source"
        warn "installing it first makes this much faster: cargo install cargo-binstall"
        echo
        # --locked so a tool's own dependency drift cannot break setup for everyone.
        cargo install --locked "${missing[@]}"
    fi
    ok "tools installed"
fi

# ---------------------------------------------------------------------------
bold "git hooks"
# ---------------------------------------------------------------------------

chmod +x .githooks/* scripts/*.sh 2>/dev/null || true

current="$(git config --get core.hooksPath || echo "")"
if [ "$current" = ".githooks" ]; then
    ok "core.hooksPath already points at .githooks"
else
    git config core.hooksPath .githooks
    ok "core.hooksPath set to .githooks"
fi

echo
echo "  hooks now active:"
echo "    pre-commit  → just check-fast   (format, clippy, tests)"
echo "    commit-msg  → Conventional Commits validation"
echo "    pre-push    → just check        (the full CI gate)"

# ---------------------------------------------------------------------------
echo
bold "verifying"
# ---------------------------------------------------------------------------

if just check-fast; then
    echo
    ok "setup complete — 'just check-fast' passes"
    echo
    echo "  next: 'just' lists every recipe, 'just check' runs the full gate"
else
    echo
    die "setup finished but 'just check-fast' failed — see the output above"
fi
