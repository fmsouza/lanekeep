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
   packages, and publishes.

Nothing publishes on a push to a branch. Nothing publishes without a tag.

## What has to exist first

Publishing is gated on two secrets, and both are absent until someone adds them:

| Secret | For |
| --- | --- |
| `NPM_TOKEN` | `npm publish`, with provenance |
| `CARGO_REGISTRY_TOKEN` | `cargo publish` |

**A tag with neither secret set still builds and packages everything** — it just publishes
nothing. That is the intended way to rehearse a release, and it means the first real publish
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
  published. The order is written out in `release.yml` rather than derived, so a new crate
  has to be placed deliberately.

`cargo publish` runs **without** `--no-verify`. A crate that cannot build from its own
published form is a crate nobody can use, and the only moment that is cheap to discover is
before it is published.

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

## Adding a platform

Three places, and all three are checked:

1. A matrix entry in `release.yml`.
2. A row in `scripts/build-npm-packages.sh`.
3. An entry in `npm/lanekeep/resolve.js` and in the launcher's `optionalDependencies`.

The packaging script fails if the launcher declares a platform that was not built, and a
test asserts the two lists agree — an install that succeeds and then cannot run is the
failure this prevents.

## Nightly

`nightly.yml` runs the full gate, the tests against the *newest* dependencies the manifests
permit, an advisory audit, and a cross-platform release build.

It exists for the failures that do not arrive with a commit: a yanked dependency, a new lint
on the stable channel, an advisory published overnight, a semver-compatible release that
broke something. A pull request would never see any of them — the committed lockfile hides
the dependency ones by design, which is right for a pull request and wrong forever.

The gate job is deliberately **uncached**. A nightly run exists to find what changed
underneath the project, and a cache is exactly what would hide it.
