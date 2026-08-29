# Contributing to lanekeep

Contributions are welcome. This document covers setup and process; if you are looking for
how the tool works, start with [`docs/architecture.md`](docs/architecture.md).
Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

If you are a coding agent, or directing one, read [`AGENTS.md`](AGENTS.md) — it has the
same process plus the invariants and the traps.

> **lanekeep is released and in use.** Every milestone in the architecture is delivered,
> and it ships on five distribution channels. The design is settled, so opening an issue
> before a large change saves you from building something it already answers differently.

## Setup

You need [rustup](https://rustup.rs). Everything else the gate needs is installed for you.

```bash
git clone https://github.com/fmsouza/lanekeep
cd lanekeep
./scripts/setup-dev.sh
```

That installs the pinned toolchain and the development tools, activates the git hooks,
and verifies the result by running the fast gate. Installing
[`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall) first makes it much
quicker — the script uses it when present and falls back to compiling from source.

**The WebAssembly rule components are build products, not committed files**, so the *first*
build of `lanekeep-rules` on a fresh checkout produces them into
`crates/lanekeep-rules/components/` — and reaches for the component toolchains to do it.
`cargo component`, installed by the setup script, covers the two Rust rules; the Go component
additionally needs Go, TinyGo and `wasm-tools` (which TinyGo shells out to — the setup script
installs `wasm-tools`, but TinyGo cannot be installed from cargo:
[`docs/authoring-go-rules.md`](docs/authoring-go-rules.md) says what to install) —
and the test-only TypeScript component needs Node. Every build after the first finds the
artifacts present and is a strict no-op, `just test-go` still skips its checks where `go`
is absent, and in CI a dedicated `components` job builds once for every other job to
download.

## Commands

Every check is defined once, in the `justfile`. CI runs the same recipes, so a green
`just check` locally means a green pull request.

| Command | Use |
| --- | --- |
| `just` | List every recipe |
| `just check-fast` | Format, clippy, tests — runs on every commit |
| `just check` | The full gate — runs on push and in CI |
| `just test` | Rust tests only |
| `just test-scripts` | The repository's own shell tooling |
| `just test-go` | The Go launcher and the Go rule SDK, skipped where Go is absent |
| `just fmt` | Apply formatting |
| `just msrv` | Verify the declared MSRV still builds |
| `just snapshot` | Review pending snapshot changes |

The git hooks run these for you. `--no-verify` exists, but the gate will catch it later
and less conveniently.

## Making a change

**Write the test first.** Watch it fail for the reason you expect before making it pass.
A test that passes on its first run has not told you anything.

**Keep pull requests small.** One concern each. A pull request that changes the cache
format *and* adds a rule is two reviews wearing one hat.

**Branch, never push to `main`.** It is protected and will reject a direct push.

## Commit and pull request titles

We use [Conventional Commits](https://www.conventionalcommits.org/).

`main` accepts squash merges only, which means **your pull request title becomes the
commit message on `main`** — and release-plz reads it to decide the next version. A
branch of well-formed commits behind a malformed title still produces a malformed
history and a wrong release.

```
feat(core): add violation and rule card types
fix(cache): include tracked reads in the entry key
docs: explain why breaching a timeout cancels the run
feat(js)!: replace the node handle representation
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`,
`chore`, `revert`. A `!` or a `BREAKING CHANGE:` footer marks a breaking change.
The subject line — type, scope and all — must be **72 characters or fewer**, and the
description after the colon starts lowercase. `./scripts/lint-commit-msg.sh --message
"your title"` answers before CI does.

`feat`, `fix`, `perf` and `revert` propose a release; everything else does not. So if your
change affects users, do not file it as a `chore` — see the `release_commits` note in
[`docs/releasing.md`](docs/releasing.md).

The `commit-msg` hook validates branch commits and CI validates the title. Both run the
same script, so they cannot disagree.

## What makes a change easy to accept

- **A test that fails without it.** This is most of it.
- **A reason, not just a change.** The pull request body should say what problem this
  solves. "Why" is the part reviewers cannot reconstruct.
- **Something said about the trade-off.** Every real change costs something. Naming the
  cost yourself is faster than having it found.
- **Documentation in the same pull request**, if behavior changed. Documentation that
  lands separately usually does not land.

## What will get pushed back

- Relaxing a check to make a change pass. Fix the change, or change the check
  deliberately and explain why.
- A new dependency without justification. The dependency surface is kept small on
  purpose — see §13 of the architecture. Network crates are banned outright by
  `deny.toml`.
- Anything breaking an invariant in [`AGENTS.md`](AGENTS.md) without a matching update to
  `docs/architecture.md`. Those invariants exist because breaking them produces bugs that
  do not announce themselves.
- Documentation for code that does not exist yet.

## Adding a rule

Start from a working example: `lanekeep init` scaffolds a runnable rule, and
[`docs/built-in-rules.md`](docs/built-in-rules.md) walks through the thirteen that ship, including
what each one deliberately does *not* catch. [`docs/cross-file-rules.md`](docs/cross-file-rules.md)
covers rules needing a whole-corpus view.

The reference is §4 of [`docs/architecture.md`](docs/architecture.md) for the rule format and §6
for the host API. The TypeScript definitions in [`packages/lanekeep`](packages/lanekeep) are
generated from the same WIT world the engine implements, so your editor's autocomplete for
`ctx` and §6 cannot disagree.

A new built-in rule needs a `RuleTester` suite driven through the real engine — see
`crates/lanekeep-rules/tests/` — and an entry in `docs/built-in-rules.md` in the same pull
request.

## Reporting a bug

Open an issue with the version, the input that triggers it, what you expected, and what
happened. A minimal reproduction is worth more than a description.

**Security issues do not go in the issue tracker.** See [`SECURITY.md`](SECURITY.md).

## License

lanekeep is dual-licensed under MIT and Apache-2.0. Unless you state otherwise, any
contribution you intentionally submit for inclusion — as defined in the Apache-2.0
license — is dual licensed the same way, with no additional terms.

There is no contributor license agreement to sign.
