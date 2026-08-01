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
   packages and the downloadable archives, publishes to both registries, and attaches the
   archives to the release.

Nothing publishes on a push to a branch. Nothing publishes without a tag.

`release-plz.yml` is steps 1 and 2; `release.yml` is step 3. They share no state but the tag.

## What has to exist first

Publishing is gated on secrets, and each is absent until someone adds it:

| Secret | For |
| --- | --- |
| `NPM_TOKEN` | `npm publish`, with provenance |
| `CARGO_REGISTRY_TOKEN` | `cargo publish` |
| `RELEASE_PLZ_TOKEN` | Letting the tag release-plz pushes trigger `release.yml` |

`NPM_TOKEN` must be a granular access token with **Bypass 2FA** enabled. Without it npm
answers `EOTP` and asks for a one-time password, which nothing in CI can supply. crates.io
requires a **verified email address** on the account before it accepts any publish at all.

`RELEASE_PLZ_TOKEN` is the one whose absence is quiet. A tag pushed with the default
`GITHUB_TOKEN` **does not trigger other workflows** — GitHub suppresses that so a workflow
cannot set itself off forever. So without it, release-plz tags and creates the release,
`release.yml` never runs, and the releases page advertises a version that is on neither
registry. `release-plz.yml` says which token it used on every run, and warns when it is the
default one. A tag pushed by hand always triggers `release.yml`.

**A tag with no registry secrets set still builds and packages everything** — it just
publishes nothing. That is the intended way to rehearse a release, and it means the first real publish
is a decision someone makes rather than one the workflow makes for them.

The `publish` job also uses a `release` [environment], so a required reviewer can be
configured on it. Worth doing before the first release: it turns publishing into something
that needs a human, on the one workflow where an accident is not undoable.

[environment]: https://docs.github.com/en/actions/deployment/targeting-different-environments/using-environments-for-deployment

## Publishing order

Both registries care about order, in opposite directions:

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

Both publishes **skip anything already on the registry**, because neither registry lets a
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

Four places, and all four are checked:

1. A matrix entry in `release.yml`.
2. A row in `scripts/build-npm-packages.sh`.
3. A row in `scripts/build-release-archives.sh`.
4. An entry in `npm/lanekeep/resolve.js` and in the launcher's `optionalDependencies`.

Both packaging scripts fail if a platform they expect was not built, and a test asserts the
launcher's list and the resolver's agree — an install that succeeds and then cannot run is the
failure this prevents. Miss (3) and the release simply has no archive for that platform, which
is why that script errors on a missing binary rather than skipping it.

## Nightly

`nightly.yml` runs the full gate, the tests against the *newest* dependencies the manifests
permit, an advisory audit, and a cross-platform release build.

It exists for the failures that do not arrive with a commit: a yanked dependency, a new lint
on the stable channel, an advisory published overnight, a semver-compatible release that
broke something. A pull request would never see any of them — the committed lockfile hides
the dependency ones by design, which is right for a pull request and wrong forever.

The gate job is deliberately **uncached**. A nightly run exists to find what changed
underneath the project, and a cache is exactly what would hide it.
