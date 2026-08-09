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
check-fast: fmt-check lint test test-rust-rules test-scripts test-go

# Full gate. What CI runs and what pre-push runs. If this is green, the PR is green.
check: fmt-check lint test test-rust-rules test-scripts test-go docs deny machete typos-check msrv

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
# The last step records what everything was built from, and it is what keeps the trade
# above honest: without it, editing a fixture's source and not running this recipe leaves
# every test in `lanekeep-wasm` asserting against the previous binary, green and
# meaningless. See `crates/lanekeep-wasm/tests/fixture_currency.rs`.
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
    built=0
    for dir in crates/lanekeep-wasm/tests/fixtures/*/; do
        [ -f "${dir}Cargo.toml" ] || continue
        name="$(basename "${dir}")"
        echo "building ${name}"
        built=$((built + 1))
        (cd "${dir}" && cargo component build --release --target wasm32-unknown-unknown)
        # cargo names the artifact after the crate with hyphens turned into underscores,
        # and the directory keeps the hyphens. `spike` has neither, so this only starts
        # mattering with the second fixture — a `cp` of a path that never existed.
        cp "${dir}target/wasm32-unknown-unknown/release/${name//-/_}.wasm" \
           "${root}/crates/lanekeep-wasm/tests/fixtures/${name}.wasm"
    done

    # A glob that matches no package builds nothing and says so in no way at all — and the
    # step below would then record the artifacts already in the tree as current, which is the
    # one thing this recipe must never do quietly.
    if [ "${built}" -eq 0 ]; then
        echo "error: no fixture crates under crates/lanekeep-wasm/tests/fixtures/." >&2
        echo "       nothing was rebuilt, so nothing may be re-recorded." >&2
        exit 1
    fi

    # And exactly one artifact built for the target every line above exists to avoid.
    #
    # `tests/load.rs` asserts that the load-time import check rejects a wrongly-targeted
    # component, and a check like that is only worth as much as the artifact it is pointed
    # at. Built here rather than described, because `AGENTS.md` records that a guest small
    # enough to allocate nothing has zero imports on *both* targets — so the difference has
    # to be produced to be believed.
    #
    # It sits one directory deeper than its siblings so the loop above cannot pick it up and
    # build it for the right target, and so `tests/world_shape.rs`'s glob over
    # `tests/fixtures/*.wasm` — which asserts every artifact it finds imports nothing but the
    # host interface — does not find the one artifact that must fail that assertion.
    echo "building wasip1 (deliberately for wasm32-wasip1)"
    dir="crates/lanekeep-wasm/tests/fixtures/rejected/wasip1/"
    (cd "${dir}" && cargo component build --release --target wasm32-wasip1)
    cp "${dir}target/wasm32-wasip1/release/wasip1.wasm" \
       "${root}/crates/lanekeep-wasm/tests/fixtures/rejected/wasip1.wasm"

    # Record what all of that was built from, so the gate can tell a stale artifact from a
    # current one without needing `cargo component` to find out.
    #
    # `cargo test` rather than `cargo nextest run`, and not because of the runner: nextest
    # runs each test in its own process with no way to say "this one writes a file", and the
    # point here is the side effect rather than the verdict. The same test asserts under
    # `just test` with the variable unset.
    echo "recording fixture digests"
    LANEKEEP_BLESS_WASM_FIXTURES=1 cargo test --quiet -p lanekeep-wasm \
        --test fixture_currency -- --exact every_committed_artifact_is_the_one_its_sources_build

# Build and test rust-rules/: its own workspace, so `cargo test --workspace` at the root does
# not reach it.
#
# Part of both gates, unlike `wasm-fixtures` — that recipe is excluded because it needs
# `cargo component` and rewrites committed artifacts, neither of which is true here: this is
# a zero-dependency host-target test that runs in under a second, and leaving it opt-in would
# mean nothing here runs until someone remembers to ask for it by name. Extending the gate
# further — deny, machete, fmt, msrv — over this second workspace is still separate, later
# work.
#
# Plain `cargo test` rather than `cargo nextest run`: the crates here are host-target unit
# tests over pure functions, with no wasm target and no fixture side effects to isolate a
# process per test for, and `cargo test` also runs doctests in the same pass that nextest
# would need a second invocation for.
test-rust-rules:
    cargo test --manifest-path rust-rules/Cargo.toml --workspace --all-features

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
