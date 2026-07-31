# Working in lanekeep

This is the source of truth for how to work in this repository, for coding agents and
humans alike. Vendor-specific files (`CLAUDE.md`, `.cursor/rules/`, `GEMINI.md`,
`.github/copilot-instructions.md`) are thin pointers to this one — if you delete every
one of them, nothing is lost.

## What this is

lanekeep checks code against *project-specific architectural conventions* — the ones a
language model cannot infer from the code it is shown. Rules are TypeScript programs
executed in an embedded sandbox; the matching that decides when they run happens in Rust.

Read [`docs/architecture.md`](docs/architecture.md) before making a design decision. It
is short and it is the authority. This file tells you how to work; that one tells you
what is being built and why.

## Commands

Every check is defined once, in the `justfile`. CI invokes the same recipes, so if it
passes here it passes there.

| Command | Use |
| --- | --- |
| `just` | List every recipe |
| `just setup` | One-time: install tooling, activate git hooks |
| `just check-fast` | Format, clippy, tests. What pre-commit runs. |
| `just check` | The full gate. What pre-push and CI run. |
| `just test` | Rust tests via nextest |
| `just test -- <filter>` | A subset — `just test -- cache::` |
| `just fmt` | Apply formatting |
| `just snapshot` | Review pending insta snapshots |
| `just msrv` | Verify the declared MSRV still builds |

**Do not invent equivalent commands.** If you find yourself typing a bare `cargo clippy`
with different flags than the `lint` recipe, either the recipe is wrong — change it — or
you are about to produce a result CI will disagree with.

## Invariants

These are not style preferences. Each one, if broken, produces a class of bug that does
not announce itself. Breaking one deliberately requires updating
[`docs/architecture.md`](docs/architecture.md) in the same change.

### The reduce phase never touches parse trees

Cross-file rules consume facts and the file list, nothing else. Facts are small and
serializable, which is what keeps cross-file rules parallel and cacheable. Handing a
tree to `reduce` would make the whole corpus resident and kill incrementality.

### Everything is deterministic given `(bytes, path, ruleset, config, tracked reads)`

Two runs over identical input produce byte-identical output. An agent reads lanekeep's
output twice and must not see reordering as change. This is why the sandbox withholds
`Math.random`, `Date.now` and `new Date()` — a rule must not be able to introduce
nondeterminism even by accident — and why violations are always sorted by
`(ruleId, file, line, column)`.

If you add anything that observes the environment, you have broken the cache. There is
no "it's only used for logging" exception; a cached result is a cached result.

### JavaScript executes proportional to matches, never to nodes

The tree-sitter query is a gate. Rust matches it; only matches cross into the sandbox.
Per-node dispatch would cross the boundary tens of thousands of times per file, which is
the cost that makes native tooling with JS plugins slow. This is the reason a Rust engine
earns its place at all — parsing was the dominant cost, and paying it back at the
boundary would leave nothing.

Any change that increases boundary crossings per file needs a benchmark, not an argument.

### The sandbox has no ambient authority

Rule code reaches exactly the host API and nothing else. `fs`, `process`,
`child_process`, network, dynamic `import()`, timers and the clock are not *restricted* —
they are absent from the context.

Adding a host function widens the trust boundary. It also bumps the host API version,
which is a cache key input, because a cached result computed without that function is not
a valid result for a run that has it.

### Limits cancel the run; they never degrade it

Breaching the per-invocation timeout, the global run budget, or the memory ceiling aborts
everything and exits `2`. Skipping the offending rule and continuing looks friendlier and
is wrong: a timeout is timing-dependent, so a rule tripping on a loaded machine and not on
an idle one makes output vary between runs on identical input.

Cache entries for files that completed in full are still committed — otherwise a corpus
that times out on a cold run times out identically forever, with no way to make progress.

### The one-way doors

Listed in [`docs/architecture.md`](docs/architecture.md) §14. In short: namespaced rule
IDs, a versioned host API in the cache key, tracked effects from the start, AST nodes
crossing as handles rather than objects, and a clean `Rule` boundary that built-ins get
no exemption from.

## Repository map

```
crates/
  lanekeep-core      engine: walker, query evaluation, facts, violations, Rule trait
  lanekeep-js        the sandbox: QuickJS, host API, TS stripping, module loader
  lanekeep-query     tree-sitter query parsing and compilation
  lanekeep-lang      Language trait and registry
  lanekeep-lang-js   TS/TSX/JS/JSX grammars, binding resolution
  lanekeep-config    config loading, rule graph resolution, hashing
  lanekeep-cache     content-addressed store with dependency tracking
  lanekeep-rules     built-in rules, authored in TypeScript
  lanekeep-report    human, json, sarif, agent reporters
  lanekeep-testkit   RuleTester
  lanekeep-cli       the binary
docs/                architecture, playbooks
scripts/             repository tooling, with its own tests
.githooks/           committed hooks, activated by `just setup`
```

Each crate's `lib.rs` opens with what it owns and, where it matters, the invariant it
exists to protect. Read that before adding to a crate.

Dependency edges are added when code needs them, not up front — an unused dependency
fails `cargo machete`.

## Making a change

**Test first.** Write a failing test, watch it fail for the reason you expect, then make
it pass. A test that passes the first time you run it has told you nothing.

**Small pull requests.** One concern each. The delivery sequence is a list of them.

**Conventional Commits, on the pull request title.** `main` takes squash merges only, so
the title becomes the commit on `main`, and release-plz reads it to pick the next
version. Branch commits are validated by the `commit-msg` hook; the title is validated by
CI. `feat` and `fix` move the version, `feat!` or a `BREAKING CHANGE` footer moves it
further.

**Never push to `main`.** It is protected and will reject you. Branch, open a pull
request, let it squash-merge.

**Run `just check` before pushing.** The pre-push hook does it anyway; running it first
saves the round trip.

## Traps already found here

Real ones, each of which cost time. If you hit something surprising, add it.

**`rust-toolchain.toml` overrides the installed default.** A CI job that installs 1.85
and runs plain `cargo check` tests 1.95 and passes regardless — a green check asserting
nothing. Only `cargo +<version>` overrides the pin. This is why `just msrv` exists.

**Most interesting rustfmt options are nightly-only.** `imports_granularity`,
`group_imports`, `wrap_comments` and friends emit a warning per file on stable and change
nothing. `rustfmt.toml` is stable-only on purpose; do not add them back.

**`typos` runs with `locale = "en-us"`.** Write `behavior`, `analyze`, `capitalize` — the
American forms — in prose and comments alike. British variants fail the gate. This is
deliberate: Rust's own vocabulary is American (`serialize`, `color`), and consistency
beats preference.

This version of `typos` honors no inline suppression directive, so a word cannot be
exempted on a single line. The choices are to rephrase or to allow it globally in
`typos.toml`, and rephrasing is almost always right — including for this very paragraph,
which cannot name the spellings it is warning you about.

**Clippy's `doc_markdown` fires on product names.** `QuickJS`, `SARIF`, `TSX` and
similar go in `clippy.toml`'s `doc-valid-idents`, not in backticks — backticks would
claim they are code identifiers.

**`unwrap`, `expect` and `panic!` are denied, but allowed in tests.** The workspace denies
them because an engine that panics on a malformed input file has failed at its job. In a
test, panicking *is* the failure mechanism, so `clippy.toml` sets `allow-*-in-tests`.
Do not add `#[allow]` attributes to test modules; the config already handles it.

**An MSRV moves when a dependency forces it, never for syntax.** It is a promise to users.
The floor went 1.85 → 1.87 for rquickjs and 1.87 → 1.88 for `ignore`, both because those
crates would not build otherwise. It did *not* move when let-chains would have been
convenient — that code was rewritten instead. `just check` runs `just msrv`, so a violation
fails locally rather than costing a CI round trip; without it, every other recipe runs on a
toolchain far newer than the floor and happily accepts syntax that does not exist there.

**`Path::is_absolute` is platform-specific, and tests that assume otherwise pass on
macOS and fail on Windows.** `/etc/passwd` is absolute on Unix and merely *rooted* on
Windows; `C:\...` is the reverse. Build an absolute path from `std::env::temp_dir()` rather
than writing a literal, or the same input takes a different code branch on each platform.

**nextest runs with `--no-tests=warn`.** Crate skeletons exist ahead of their milestones.
Tighten this to `fail` once M0 lands and every crate has behavior to assert.

**A stacked pull request conflicts as soon as its parent is squash-merged.** Squashing
replaces the parent's commits with one new commit that has a different SHA, so git sees the
child branch and `main` as having added the same files independently. Every file conflicts
even though no content disagrees. The fix is not to resolve anything by hand:

```bash
git fetch origin
git rebase --onto origin/main <last-commit-of-the-parent-branch>
git push --force-with-lease
```

That replays only the child's own commits onto the new `main` and drops the duplicate.

## What not to do

- Do not add a dependency without checking `deny.toml`. Network crates are banned
  outright; the "no network, ever" claim in §13 is enforced, not aspirational.
- Do not add a GitHub Action pinned to a tag. SHAs only, with the version in a trailing
  comment so Dependabot can bump it.
- Do not interpolate `${{ }}` inside a workflow `run:` block. It is substituted before
  bash sees it, so attacker-controlled text like a pull request title would execute. Pass
  it through `env:`.
- Do not relax a check to make a change pass. Fix the change, or change the check
  deliberately and say why in the pull request.
- Do not write documentation for code that does not exist yet. Playbooks ship in the same
  pull request as the thing they describe.
