# lanekeep task definitions.
#
# This file is the single source of truth for what every check means. Git hooks and
# CI both invoke these recipes rather than spelling out their own commands, so local
# and CI cannot drift: if `just check` passes on your machine, it passes in CI, and
# a change to a check happens in exactly one place.

set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-load := false

# Show available recipes.
default:
    @just --list --unsorted

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

# Install development tooling and activate the committed git hooks.
setup:
    @./scripts/setup-dev.sh

# Fail with an actionable message when a required tool is missing.
_require tool:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v {{ tool }} >/dev/null 2>&1; then
        echo "error: '{{ tool }}' is not installed." >&2
        echo "       run 'just setup' to install the full development toolchain." >&2
        exit 1
    fi

# ---------------------------------------------------------------------------
# The two gates. Everything else is a component of one of them.
# ---------------------------------------------------------------------------

# Pre-commit gate. Fast enough to run on every commit without being resented.
check-fast: fmt-check lint test test-scripts test-go

# Full gate. What CI runs and what pre-push runs. If this is green, the PR is green.
check: fmt-check lint test test-scripts test-go docs deny machete typos-check msrv

# ---------------------------------------------------------------------------
# Components
# ---------------------------------------------------------------------------

# Apply formatting.
fmt:
    cargo fmt --all

# Verify formatting without changing anything.
fmt-check:
    cargo fmt --all -- --check

# Clippy across every target, warnings are errors.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the Rust test suite.
#
# `--no-tests=warn` rather than the default `fail`: the crate skeletons exist ahead of
# their milestones and legitimately contain no tests yet, but a silent pass would hide
# a genuinely empty suite. Warning keeps that visible. Tighten to `fail` when M0 lands
# and every crate has behavior to assert.
test *ARGS:
    @just _require cargo-nextest
    cargo nextest run --workspace --all-features --no-tests=warn {{ ARGS }}

# Doctests, which nextest does not run.
test-doc:
    cargo test --workspace --doc

# Rebuild the committed WebAssembly test fixtures from their sources.
#
# Deliberately not part of any gate. The built components are committed, so the gate does
# not need `cargo component` and CI does not install it; this is what you run after
# changing a fixture's WIT or source, and the resulting `.wasm` is reviewed like any other
# change.
#
# `--target wasm32-unknown-unknown` is a requirement, not a preference. `cargo component`
# defaults to `wasm32-wasip1`, whose components import a wall clock and a filesystem as
# soon as the guest touches anything in `std` — the two capabilities the sandbox exists to
# withhold. On this target the import list is exactly the declared world.
wasm-fixtures:
    #!/usr/bin/env bash
    set -euo pipefail
    just _require cargo-component
    root="$(pwd)"
    for dir in crates/lanekeep-wasm/tests/fixtures/*/; do
        [ -f "${dir}Cargo.toml" ] || continue
        name="$(basename "${dir}")"
        echo "building ${name}"
        (cd "${dir}" && cargo component build --release --target wasm32-unknown-unknown)
        # cargo names the artifact after the crate with hyphens turned into underscores,
        # and the directory keeps the hyphens. `spike` has neither, so this only starts
        # mattering with the second fixture — a `cp` of a path that never existed.
        cp "${dir}target/wasm32-unknown-unknown/release/${name//-/_}.wasm" \
           "${root}/crates/lanekeep-wasm/tests/fixtures/${name}.wasm"
    done

# Tests for the repository's own shell tooling.
#
# lint-commit-msg.sh gates every commit and every pull request title, and the title is
# what release-plz reads to pick the next version. A false accept ships a wrong release;
# a false reject blocks everyone. It is not too small to test.
test-scripts:
    @./scripts/test-lint-commit-msg.sh
    @./scripts/test-build-npm-packages.sh
    @./scripts/test-build-release-archives.sh
    @./scripts/test-build-homebrew-formula.sh
    @./scripts/test-build-python-wheels.sh
    @./scripts/test-publish-npm.sh
    @./scripts/test-publish-crates.sh
    @./scripts/test-publish-pypi.sh
    @./scripts/test-shell-portability.sh
    @./scripts/test-workflows.sh
    @./scripts/test-release-config.sh

# The Go launcher: formatting, vet, and its own tests.
#
# Skipped where Go is absent rather than failing. It is one distribution lane, and making
# the Rust gate require a Go toolchain would cost every contributor for something most of
# them never touch — the same trade `test-shell-portability.sh` makes for bash 3.2. CI has
# Go on every runner, so it is a real check there.
test-go:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v go >/dev/null 2>&1; then
        echo "note: no go toolchain here, so the launcher tests are skipped (CI covers them)"
        exit 0
    fi
    unformatted="$(gofmt -l ./cmd)"
    if [ -n "${unformatted}" ]; then
        echo "error: not gofmt'd:" >&2
        echo "${unformatted}" >&2
        exit 1
    fi
    go vet ./...
    go test ./...

# Build documentation the way docs.rs will, failing on broken intra-doc links.
docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features

# Supply-chain policy: advisories, licenses, banned crates, source provenance.
deny:
    @just _require cargo-deny
    cargo deny --all-features check

# Known vulnerabilities in the dependency graph.
audit:
    @just _require cargo-audit
    cargo audit --deny warnings

# Dependencies declared but never used.
machete:
    @just _require cargo-machete
    cargo machete --with-metadata

# Spelling, across code and prose.
typos-check:
    @just _require typos
    typos

# Fix the spelling mistakes that can be fixed automatically.
typos-fix:
    @just _require typos
    typos --write-changes

# Verify the crate still builds on the MSRV declared in Cargo.toml.
#
# Part of `just check`, and it has to be. The pinned toolchain is far newer than the MSRV,
# so every other recipe happily accepts syntax that does not exist on the floor we promise —
# let-chains being the one that caught us out. Without this in the gate, "it passes locally"
# is false for exactly the failures hardest to guess at.
msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    version=$(grep -m1 '^rust-version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')
    echo "checking against declared MSRV ${version}"
    rustup toolchain install "${version}" --profile minimal --no-self-update
    cargo "+${version}" check --workspace --all-features --all-targets

# ---------------------------------------------------------------------------
# Benchmarks and snapshots
# ---------------------------------------------------------------------------

# Run the benchmark suite against the fixture corpus.
#
# Prints each scenario against its budget from docs/architecture.md §15, and fails only
# past 4x — see the bench's own documentation for why the gate is loose and the report is
# not.
bench *ARGS:
    cargo bench --workspace {{ ARGS }}

# Review pending insta snapshots interactively.
snapshot:
    @just _require cargo-insta
    cargo insta review

# Accept every pending snapshot. Read the diff first — this is how a wrong
# expectation becomes the committed expectation.
snapshot-accept:
    @just _require cargo-insta
    cargo insta accept

# ---------------------------------------------------------------------------
# Slower analyses. Nightly in CI rather than per-pull-request.
# ---------------------------------------------------------------------------

# Mutation testing: are the tests actually asserting anything?
mutants *ARGS:
    @just _require cargo-mutants
    cargo mutants --workspace {{ ARGS }}

# Fuzz a target for a bounded time. `just fuzz config 60`
fuzz target seconds="60":
    @just _require cargo-fuzz
    cargo fuzz run {{ target }} -- -max_total_time={{ seconds }}

# ---------------------------------------------------------------------------
# Housekeeping
# ---------------------------------------------------------------------------

# Remove build output and lanekeep's own cache.
clean:
    cargo clean
    rm -rf .lanekeep

# Validate a commit message against Conventional Commits. Used by the commit-msg hook.
lint-commit-msg file:
    @./scripts/lint-commit-msg.sh {{ file }}
