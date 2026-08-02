# Releasing

A release is three things: a version bump, a tag, and a publish. They are deliberately
separate, so each is reviewable on its own and none can happen by accident.

## The shape

1. **release-plz opens a release pull request.** It reads the Conventional Commits on `main`
   — which, because `main` takes squash merges only, are the pull request titles — and
   proposes the next version with a changelog.
2. **Merging it creates a tag.** That is the decision point. Everything before it is
   reversible by closing a pull request.
3. **The tag runs `release.yml`**, which builds every platform binary, assembles the npm
   packages, the Python wheels and the downloadable archives, publishes to every configured
   index, and attaches the archives to the release.

Nothing publishes on a push to a branch. Nothing publishes without a tag.

`release-plz.yml` is steps 1 and 2; `release.yml` is step 3. **They share no state but the tag
name**, and nothing makes them agree on it, so it is pinned in both directions: release-plz is
configured to tag `v{{ version }}`, `release.yml` triggers on `v*`, and
`scripts/test-release-config.sh` fails if either drifts.

That is not hypothetical. release-plz defaults to `{{ package }}-v{{ version }}` for a
workspace with more than one public crate, and v0.1.1 shipped with that default half-overridden
— releases were disabled for the eleven library crates but tagging never was. The result was
twelve tags nobody reads and a release called `lanekeep-cli-v0.1.1`, while `release.yml`
attached its binaries to `v0.1.1`. Both halves succeeded; the release was still wrong.

## What has to exist first

Publishing is gated on secrets, and each is absent until someone adds it:

| Secret | For |
| --- | --- |
| `NPM_TOKEN` | `npm publish`, with provenance |
| `CARGO_REGISTRY_TOKEN` | `cargo publish` |
| `PYPI_TOKEN` | `twine upload` of the wheels |
| `RELEASE_PLZ_TOKEN` | Letting release-plz open the release pull request, and letting the tag it pushes trigger `release.yml` |
| `HOMEBREW_TAP_TOKEN` | Pushing the generated formula to the tap |

`NPM_TOKEN` must be a granular access token with **Bypass 2FA** enabled. Without it npm
answers `EOTP` and asks for a one-time password, which nothing in CI can supply. crates.io
requires a **verified email address** on the account before it accepts any publish at all.

`PYPI_TOKEN` is an API token from [PyPI account settings]. Before the project exists there is
nothing to scope a token *to*, so the first one has to be account-scoped ("Entire account");
once `lanekeep` is on the index, replace it with one scoped to that project alone. The token
is used as the password with `__token__` as the username, which is what `release.yml` sets.

[PyPI account settings]: https://pypi.org/manage/account/token/

`RELEASE_PLZ_TOKEN` is the one to get right, because the default token cannot do two
separate things and neither is obvious from the failure.

**It cannot open the release pull request** unless the repository allows it —
Settings → Actions → General → Workflow permissions → *Allow GitHub Actions to create and
approve pull requests*. Without that, the run fails with a 403 reading "GitHub Actions is not
permitted to create or approve pull requests", which looks like a bug in release-plz and is a
repository setting.

**It cannot trigger `release.yml` from the tag it pushes.** GitHub suppresses that so a
workflow cannot set itself off forever. So even with the setting enabled and the release pull
request merged, the tag lands, the GitHub release appears, and nothing publishes — a releases
page advertising a version that is on neither registry.

A fine-grained PAT in `RELEASE_PLZ_TOKEN`, with contents and pull-requests write, is the one
fix for both. `release-plz.yml` prints which token it used on every run and spells out both
consequences when it is the default. A tag pushed by hand always triggers `release.yml`, which
is how every release so far has happened.

**A tag with no registry secrets set still builds and packages everything** — it just
publishes nothing. That is the intended way to rehearse a release, and it means the first real publish
is a decision someone makes rather than one the workflow makes for them.

The `publish` job uses a `release` [environment] with a **required reviewer**, so publishing
waits for a human. With `RELEASE_PLZ_TOKEN` set the rest of the chain runs unattended —
merging the release pull request tags, and the tag triggers `release.yml` — and this is the
one point that stops and asks. Worth keeping: neither registry lets a version be reused, so
an accidental publish is not undoable.

Self-review is permitted, because the reviewer is also the person who merges the release
pull request; forbidding it would make the gate impossible to pass rather than careful.

`release-plz.yml` also accepts a `workflow_dispatch`, which is how to retry a run that failed
for a reason outside the code:

```sh
gh workflow run release-plz --ref main
```

It decides what to do from the commits and the registry, so a dispatch does the same work the
push would have done.

[environment]: https://docs.github.com/en/actions/deployment/targeting-different-environments/using-environments-for-deployment

## The changelog

**One `CHANGELOG.md`, at the repository root.** release-plz defaults to one per crate, and
that shape feeds a loop here: it writes them into `crates/*/`, then counts a changed file
under a crate as a change to that crate, and proposes a release for it. v0.2.0 shipped and
the next run proposed 0.2.1 with no code change at all — only the changelogs it had just
written. Twelve files were also the wrong shape for a workspace whose crates share one
version and release together: one thing documented twelve times.

`lanekeep-cli` is the only crate that writes one, and `changelog_include` names every other
crate so it records the whole workspace rather than just the binary. That matters twice over,
because **the GitHub release body is generated from the changelog** — a crate missing from
that list is a change that appears in neither.

`scripts/test-release-config.sh` compares the list against `cargo metadata`, so a crate added
later and forgotten fails the gate instead of quietly disappearing from the release notes.

## Publishing order

Two of the three care about order, in opposite directions. PyPI does not: a wheel names its
own platform, so there is no resolution step to leave half-satisfied.

- **npm**: platform packages first, then the launcher. The launcher declares them as optional
  dependencies, so publishing it first leaves a window where `npm install lanekeep` resolves
  to a launcher whose binaries do not exist yet.
- **crates.io**: dependency order, because it rejects a crate whose dependencies are not yet
  published. `scripts/publish-crates.sh` computes it from `cargo metadata`, so it cannot
  disagree with the manifests. It used to be written out by hand, on the theory that a
  computed order was one more thing that could drift — and it was the written list that
  drifted, placing `lanekeep-rules` before the `lanekeep-testkit` it dev-depends on. That
  failed on the tenth of twelve crates, with nine already published and nothing retractable.

  Dev-dependencies are part of the order, which is the part that is easy to miss: cargo
  resolves them when it packages, so a crate whose dev-dependency is unpublished cannot go up
  even though nothing it ships uses it.

All three publishes **skip anything already on the index**, because none of them lets a
version be replaced. Without that, a release that dies partway can never be finished: the
re-run stops on the first thing already published and never reaches the one that failed, so
the only way forward is a fresh version number. crates.io additionally rate-limits *new*
crates — a burst, then roughly one per ten minutes — which a first release of a twelve-crate
workspace trips every time, so a 429 is waited out. Nothing else is retried.

`cargo publish` runs **without** `--no-verify`. A crate that cannot build from its own
published form is a crate nobody can use, and the only moment that is cheap to discover is
before it is published.

## What a release attaches

`scripts/build-release-archives.sh` builds one archive per platform from the same binaries the
npm packages are cut from — one build feeds both channels, so what the releases page serves and
what npm installs are the same bytes. Windows gets a `.zip`, everything else a `.tar.gz`, and
each carries the binary, the README and both licenses, since someone who downloaded a tarball
has no other copy of either.

`SHA256SUMS` sits beside them with plain filenames, so `sha256sum -c SHA256SUMS` works in the
directory the files were downloaded into.

The upload creates the release if release-plz has not already — which is what makes a
hand-pushed tag produce a complete release — and uses `--clobber`, so re-running replaces
assets rather than failing on the ones already there.

## Homebrew

`brew install fmsouza/tap/lanekeep`, from a tap this repository pushes to rather than from
homebrew-core — core has notability requirements a new project does not meet, and a tap needs
nobody's approval.

`scripts/build-homebrew-formula.sh` generates the formula from the archives' own `SHA256SUMS`
rather than recomputing hashes. That file is what a user verifies a download against, so a
formula built from anything else could disagree with it, and the disagreement would only
surface as a failed install.

**No Intel macOS.** The release does not build that binary, so the formula does not claim it —
a URL for an archive that was never uploaded 404s at install time, which reads as Homebrew
being broken rather than as a platform we do not ship. The formula's header names
`cargo install lanekeep-cli` instead, which is what the npm launcher does too.

The tap push needs its own secret. `RELEASE_PLZ_TOKEN` is scoped to this repository; reaching
`fmsouza/homebrew-tap` needs a token that reaches a different one. Without it the step is
skipped and the publish gate says `homebrew: no (tap not configured)`.

**Setting the tap up**, once:

1. Create a public repository named `fmsouza/homebrew-tap`. The `homebrew-` prefix is what
   makes `brew install fmsouza/tap/lanekeep` resolve; the tap is then referred to without it.
2. Give it a `Formula/` directory — the workflow creates it if absent, but an empty repository
   with a README reads better to anyone who finds it.
3. Create a fine-grained PAT with **contents: write** on that repository alone, and add it as
   `HOMEBREW_TAP_TOKEN` here.

The formula is regenerated on every release and pushed only when it differs, so a re-run of a
release that already updated the tap is a no-op rather than an empty commit.

## PyPI

`pip install lanekeep`, so a Python project can pin it in `requirements.txt` or
`pyproject.toml` the way a Node project pins it in `package.json`. lanekeep checks Python
code; this is the other half of that, and without it a Python team's only options were an npm
package needing Node or a `cargo install` needing Rust.

**One project, four wheels, no launcher.** The npm distribution needs a launcher because
npm has no notion of a platform-specific package that resolves automatically; a wheel names
its platform in its own filename and pip picks by that tag. There is nothing to resolve at
run time and nothing to get wrong.

**Nothing importable ships.** No package directory, no `__init__.py`, no console-script shim.
The binary goes in `lanekeep-<version>.data/scripts/`, which is the one location an installer
puts onto `PATH`, executable. `import lanekeep` does not work and is not meant to — a Python
module that only re-exec'd the binary would be a second thing to keep in step with the first.

`scripts/build-python-wheels.sh` assembles them and `scripts/publish-pypi.sh` uploads them,
both covered by tests that `just check` runs. The upload **skips per file, not per version**,
which is the distinction that makes a partial release resumable: a run that died halfway
leaves the version on the index with only some of its wheels, so asking "is 0.3.1 published?"
answers yes and skips exactly the work still outstanding.

### The glibc floor

The Linux wheels are tagged `manylinux_2_17`, and that tag is a promise: it says the binary
runs against glibc 2.17 and newer. Overstating it is worse than shipping nothing, because the
install succeeds and the failure arrives later as a linker error naming a library the user did
not know they had.

Nothing used to state a floor at all, so it was inherited from whatever the runner label
pointed at. `ubuntu-latest` rolled from 22.04 to 24.04, the floor went 2.35 → 2.39, and
**v0.3.1's Linux binary does not start on Ubuntu 22.04, Debian 12 or RHEL 9** — on npm, on the
releases page and in Homebrew alike. Every check was green. The smoke test runs on the machine
that built the binary, which is the one machine where the floor is never wrong.

Two things fix it, and both are needed:

- **The build states the floor.** Linux targets go through `cargo zigbuild` with a versioned
  target triple — `--target x86_64-unknown-linux-gnu.2.17` — which is the only place in this
  repository where the floor is declared rather than inherited. `scripts/test-workflows.sh`
  fails if a `linux-gnu` target is added without one.
- **The floor is checked against the binary.** `scripts/check_glibc_floor.py` parses the ELF's
  `.gnu.version_r` and refuses to tag a wheel whose binary needs more than it claims. It runs
  in the release, before anything is published, and nightly, so a toolchain regression surfaces
  on an ordinary morning.

The same binaries feed npm, the archives and Homebrew, so all four channels get the floor.

## How the npm distribution works

One package per platform, plus a launcher that depends on all of them as
`optionalDependencies`. Each platform package declares `os` and `cpu`, so npm installs
exactly one — a developer downloads one binary rather than five.

**Node is not required to run lanekeep.** It is required only to install it this way. The
binary has the JavaScript engine compiled in; the launcher exists to pick which binary.

`scripts/build-npm-packages.sh` assembles them from a release build's artifacts and rewrites
every version from the tag. Nothing about a version is committed: a committed version is one
more thing to forget, and forgetting it publishes a launcher that pulls the previous
release's binaries. `scripts/test-build-npm-packages.sh` covers the assembly and the
launcher's platform resolution, and runs as part of `just check`.

## Which platforms are prebuilt

macOS on Apple silicon, Linux on x86-64 and arm64, Windows on x86-64. Each is built on a
runner of its own architecture and smoke-tested before it ships.

## Adding a platform

Five places, and all five are checked:

1. A matrix entry in `release.yml` — with a `glibc` floor, if it is a `linux-gnu` target.
2. A row in `scripts/build-npm-packages.sh`.
3. A row in `scripts/build-release-archives.sh`.
4. A row in `scripts/build-python-wheels.sh`, with the wheel platform tag.
5. An entry in `npm/lanekeep/resolve.js` and in the launcher's `optionalDependencies`.

Every packaging script fails if a platform it expects was not built, and a test asserts the
launcher's list and the resolver's agree — an install that succeeds and then cannot run is the
failure this prevents. Miss (3) and the release simply has no archive for that platform, which
is why that script errors on a missing binary rather than skipping it. Miss the `glibc` in (1)
and `scripts/test-workflows.sh` fails.

Picking the tag in (4) is the one step with no single right answer: it has to be what pip
matches on that platform and no broader. The Linux tags are checked against the binary, so an
overstatement fails the build rather than the install.

## Nightly

`nightly.yml` runs the full gate, the tests against the *newest* dependencies the manifests
permit, an advisory audit, and a cross-platform release build.

It exists for the failures that do not arrive with a commit: a yanked dependency, a new lint
on the stable channel, an advisory published overnight, a semver-compatible release that
broke something. A pull request would never see any of them — the committed lockfile hides
the dependency ones by design, which is right for a pull request and wrong forever.

The gate job is deliberately **uncached**. A nightly run exists to find what changed
underneath the project, and a cache is exactly what would hide it.
