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
| `just test-go` | The Go launcher's tests, skipped where Go is absent |
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
  lanekeep-nodes     the node arena: parsed-tree handles shared by every rule-execution engine
  lanekeep-lang      Language trait and registry
  lanekeep-lang-js   TS/TSX/JS/JSX grammars, binding resolution
  lanekeep-lang-python  Python grammar, binding resolution
  lanekeep-lang-go      Go grammar, binding resolution
  lanekeep-lang-rust    Rust grammar, binding resolution
  lanekeep-languages    the set of supported languages, assembled in one place
  lanekeep-config    config loading, rule graph resolution, hashing
  lanekeep-cache     content-addressed store with dependency tracking
  lanekeep-wasm      WebAssembly component execution: the WIT host API, wasmtime wiring
  lanekeep-rules     built-in rules, authored in TypeScript
  lanekeep-report    human, json, sarif, agent reporters
  lanekeep-server    LSP and MCP over stdio, JSON-RPC by hand
  lanekeep-testkit   RuleTester
  lanekeep-cli       the binary
rust-rules/          a second Cargo workspace: rule crates authored in Rust
  lanekeep-rule      the SDK they share: a capture lookup and a glob matcher
cmd/lanekeep/        the Go launcher, so `go tool lanekeep` works
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

**The test suite can set `core.bare = true` on the real repository, and every later git command
then fails with "this operation must be run in a work tree."** `changed.rs` and
`tests/incremental.rs` build throwaway repositories with `git -C <tmpdir> init`. That is safe from
a plain shell and unsafe from anything that exports `GIT_DIR` — a git hook does, which is why it
shows up when the gate runs under `pre-push` rather than when you run `just check` by hand. `-C`
changes the working directory; it does not override `GIT_DIR`, so `init` initializes the *real*
repository, finds no worktree relationship, and records it as bare. Worktrees are hit hardest,
since they resolve through the common config.

The repair is one command, run from the main checkout:

```bash
git config --file .git/config core.bare false
```

Nothing is lost — no objects or refs are touched, only that one line. The durable fix is for those
helpers to clear the variables they must not inherit (`.env_remove("GIT_DIR")`,
`.env_remove("GIT_WORK_TREE")`), which makes them hermetic no matter who invokes them.

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
Do not add `#[allow]` attributes to `#[cfg(test)]` modules; the config already handles it.

The grant reaches `#[test]` functions and `#[cfg(test)]` modules — and nothing else. A
helper in a `tests/` integration crate is neither, so `fn tester() { ... .expect(...) }`
fails the gate there while the identical code passes in a unit test module. Restate the
grant with a crate-level `#![expect(...)]` carrying a `reason`, rather than rewriting test
scaffolding to thread `Result` through every helper. List only the lints that actually fire
— `expect` is an error when unfulfilled, and a `panic!` inside a `#[test]` body is already
covered.

**An MSRV moves when a dependency forces it, never for syntax.** It is a promise to users.
The floor went 1.85 → 1.87 for rquickjs, 1.87 → 1.88 for `ignore` and 1.88 → 1.94 for
`wasmtime`, all three because those crates would not build otherwise. It did *not* move when
let-chains would have been convenient — that code was rewritten instead. `just check` runs
`just msrv`, so a violation fails locally rather than costing a CI round trip; without it,
every other recipe runs on a toolchain far newer than the floor and happily accepts syntax
that does not exist there.

The `wasmtime` move is the largest of the three and worth understanding before it is repeated.
`wasmtime` releases monthly and raises its own floor with roughly every release, so its
declared `rust-version` tracks about one release behind current stable — 47.0.3 declares
1.94.0 while the pinned toolchain is 1.95.0. The newest `wasmtime` that builds on 1.88 is
38.0.4; 39.0.0 already needs 1.89. So there is no version of this dependency that both holds
an old floor and is the version anything was measured against, and every subsequent `wasmtime`
bump will drag the floor with it. Budget for that rather than discovering it per release.

**`rayon`'s `map_init` runs its initializer per chunk, not per thread.** State built there
is not reliably shared across the items one worker handles — with a small input, rayon splits
down to single items and every item gets its own. A design that relies on per-worker state
being shared is therefore untestable at small scale and only wrong at large scale. Either
make the state per item, or accept that a test cannot distinguish the two.

**And the corollary that reads the other way round: the initializer count grows with the input,
so any "bounded by threads × N" arithmetic about it is wrong.** The entry above warns about small
inputs; the more expensive mistake is at large ones. Measured through `lanekeep-engine` on
2026-08-06, release, 14 threads, one initializer per chunk:

| files | rules | initializers |
|---|---|---|
| 2,000 | 1 | 813 |
| 2,000 | 10 | 579 |
| 10,000 | 10 | 1,038 |

Not fourteen, and not stable between runs of one corpus — the two 2,000-file rows are the same
corpus and disagree, because rayon splits on how the work is going, so this is a distribution
rather than a bound. `lanekeep-wasm`'s `MEMORY_RESERVATION` was justified on "roughly three
hundred and fifty instantiations for a run, and it does not grow with the corpus", from workers ×
rules at fourteen workers; the real figure at ten thousand files times ten rules is 10,380. The
design was right — one instance per (worker, rule) is what keeps it off files × rules, which would
be 100,000 — and the *number* it was defended with was thirty times too small. If a cost is per
initializer, measure the initializers; `with_min_len` is the lever that makes the count something
you chose.

**A tree-sitter query matches children in tree order.** `(import_statement source: (string)
(import_clause ...))` can never match, because the grammar puts `import_clause` first — and
the error is exactly that, "this pattern can never match", which is easy to read as "this
node type does not exist". Check the grammar's child order before rewriting the node names.

**A raw control character in a rule's source reports a parse failure somewhere else.** A NUL
written into a template literal made the stripper report an error at the enclosing
`return`, twenty lines earlier, because that is where the outermost `ERROR` node starts.
Nothing is visible in an editor. If a rule fails to strip at a line that looks fine, dump the
`ERROR` node positions from the tree rather than reading the reported one — and write
control characters as escapes (`\u0000`), never literally.

**`Path::is_absolute` is platform-specific, and tests that assume otherwise pass on
macOS and fail on Windows.** `/etc/passwd` is absolute on Unix and merely *rooted* on
Windows; `C:\...` is the reverse. Build an absolute path from `std::env::temp_dir()` rather
than writing a literal, or the same input takes a different code branch on each platform.

**nextest runs with `--no-tests=warn`.** Crate skeletons exist ahead of their milestones.
Tighten this to `fail` once M0 lands and every crate has behavior to assert.

**`npm publish dist-npm/lanekeep` does not publish a directory.** npm reads a bare `<a>/<b>`
as a GitHub shorthand, so that command tries to clone
`ssh://git@github.com/dist-npm/lanekeep.git` and dies with a public-key error naming a
repository that does not exist — which reads like a credentials problem and is not one.
A leading `./` or a trailing `/` is enough to make it a path. This shipped in v0.1.0's first
release: the platform packages published only because the glob `dist-npm/@lanekeep/*/` leaves
a trailing slash on every match, and the launcher, written without one, was the single line
that failed.

**macOS ships bash 3.2, and a Mac with Homebrew's bash does not.** `mapfile`, `readarray`,
`declare -A` and `${var^^}` are all bash 4+, so a script using them passes locally and does
nothing at all on a macOS runner — the array comes back empty and every downstream assertion
fails at once. `just test-scripts` now runs the publish suites under `/bin/bash` wherever a 3.x
one exists, which on any Mac is a real check rather than a skipped one.

**bash 3.2 cannot parse a heredoc inside `$(...)` that contains an apostrophe.** It scans
command substitution without understanding the heredoc, so a `'` in an embedded Python comment
— `ZipInfo's`, `the zip's metadata` — reads as an opening quote and the *whole file* fails to
parse with "unexpected EOF while looking for matching `''". Newer bash is perfectly happy, so
nothing but a 3.2 parse notices. Redirect the heredoc to a file at statement level and read it
back, rather than capturing it with `$(...)`.

`scripts/test-shell-portability.sh` runs `bash -n` over every script with a 3.x bash when one
exists, which is what catches this. It found a latent instance in `test-workflows.sh` that had
never fired: that script exits early when pyyaml is missing, which is the case on the macOS
runner, so bash never reached the offending line. Worth knowing separately — **the workflow
checks do not run on macOS at all**, only in the ubuntu `gate` job. That is fine, since they
are static checks on YAML, but it is not what the run log looks like.

**Python's stdout writes CRLF on Windows, and CR-carrying values still compare equal to each
other.** That is what makes it quiet: a version read from a Python helper matches another value
from the same helper, so only a comparison against a literal written in the script fails, and
everything downstream of that silently stops happening. `scripts/test-shell-portability.sh`
reproduces it everywhere by putting a `python3` that appends a carriage return ahead of the real
one on `PATH`. Truncating at the first whitespace — `${value%%[[:space:]]*}` — is the fix, since
none of these values contain any.

**The grammar that parses a file is chosen by the file, not by the rule.** It was the other
way round until a real migration exposed it: a rule declaring `language: 'typescript'` — which
was the default — parsed `.tsx` files with the TypeScript grammar, every JSX element became an
`ERROR` node, and queries matched nothing inside them. Silently: no error, no warning, a tree
that "parsed". On a React Native codebase that hid most of the code and produced 2218 false
positives in one rule. `language` now takes one or several and defaults to
`['typescript', 'tsx']`, and a rule does not run on a file whose language it does not name.

**A WebAssembly component's import list depends on what the guest touches, not only on the
target it was built for — so a small fixture cannot tell you the target is right.** Rule
components must be built for `wasm32-unknown-unknown`; `cargo component`'s default is
`wasm32-wasip1`, whose components import a wall clock and two filesystem interfaces, which are
exactly the capabilities the sandbox exists to withhold. The trap is that this is invisible at
small scale. Measured 2026-08-05 on `cargo component` 0.21.1: a guest exporting
`add: func(u32, u32) -> u32` has **zero** imports on *both* targets, because it allocates
nothing and reaches no part of `std` that touches the WASI adapter. The scaffold's
`hello-world: func() -> string`, one `String` away, has **ten** on `wasm32-wasip1` — including
`wasi:clocks/wall-clock`, `wasi:filesystem/types` and `wasi:filesystem/preopens` — and zero on
`wasm32-unknown-unknown`. So a fixture built on the wrong target passes an import assertion
right up until a real rule formats a violation message. Pin the target at the build *and*
check every artifact's import list at load; neither substitutes for the other.

**`cargo publish` needs a crate's dev-dependencies on the registry too.** It resolves them when
it packages, so a crate whose dev-dependency is unpublished cannot go up even though nothing it
ships uses it. `lanekeep-rules` dev-depends on `lanekeep-testkit` for `RuleTester`, which puts
testkit earlier in the publication order than the dependency arrows in `[dependencies]` suggest.
The publication order is computed from `cargo metadata` for exactly this reason — a
hand-maintained list had them backwards and v0.1.1 died on the tenth of twelve crates, with nine
already published and no way to take them back.

**A registry publish must be resumable, because neither registry can take a version back.**
crates.io is append-only and npm refuses to republish a version, so a multi-package release
that dies partway cannot simply be re-run — it stops on the first thing already published and
never reaches the one that failed, stranding the release at that version permanently. Both
publish scripts therefore skip what the registry already has. v0.1.0 spent a cycle stuck with
four platform packages up and no launcher, which is exactly the state that motivated it.

**`git diff` does not see an untracked file, so it cannot decide whether to commit one.** A
workflow that writes a file into a checkout and asks `git diff --quiet` whether to push gets
"nothing to do" whenever that file is new — announcing success while doing the opposite of its
job. Stage first and compare `git diff --cached`. This was latent in the Homebrew tap step: the
only run that would have hit it is the first one, against a tap with no formula in it yet.

**A WIT type declared inside an interface is not in scope in a world that imports it.** The
world needs `use types.{check-context};` for every type it names in an export signature. Deleting
that line from `crates/lanekeep-wasm/wit/world.wit` gives ``name `check-context` does not exist``
under `wasm-tools` 1.255.0, pointing at the export's parameter — it names whichever type the first
offending signature mentions, and it reads as though the type is missing rather than out of scope.
A sketch carrying the bug is in `docs/superpowers/specs/2026-08-04-rust-rule-authoring-design.md`
§2.4, whose `world rule` names `rule-context` and `reduce-context` with no `use`. The sub-project's
own sketch, in `2026-08-04-wit-host-api-design.md` §3, does *not* have it: it carries the `use` and
documents the trap in a doc comment, and extracted verbatim it parses.

**Every WIT comment is a doc comment, including `//`.** `wit-parser` attaches a plain `//` block
to the item that follows exactly as it attaches `///`, so `wasm-tools component wit` prints the
whole file back with `///` on everything. There is no way to write a note that stays out of the
resolved package. What saves it from mattering is that the docs do not reach the artifact:
`wit-bindgen` 0.41.0 drops them, and a built component's embedded WIT carries none.

**A component's embedded WIT is a *subset* of the world it was built against.** The world-shape
fixture calls three of `check-context`'s twenty-four methods and its component-type section lists
those three; `node-location`, `binding-kind`, `read-error` and `fact-error` are absent entirely.
A load-time check comparing an artifact's WIT against `crates/lanekeep-wasm/wit/world.wit` would
therefore reject every real rule. The comparable thing is the set of imported instance names.

**A rustup toolchain's *name* reaches a component's bytes, so "same compiler" is not the same as
"same artifact".** Component builds are otherwise reproducible here — measured 2026-08-06, the nine
`wasm32-unknown-unknown` fixtures in `crates/lanekeep-wasm/tests/fixtures/` that existed then
(`engine-rule` arrived after, making ten) built twice from clean with `cargo component` 0.21.1
produced nine pairs of byte-identical artifacts, each matching what is committed, and the
checkout's absolute path does not appear in any of them. What does appear is a standard-library source path in a panic location, which carries the
*toolchain directory*: `.../toolchains/stable-aarch64-apple-darwin/...` against
`.../toolchains/1.95.0-aarch64-apple-darwin/...`. Building `world-shape` through `stable` rather
than through the pin gave a different sha256 at the same size, differing in exactly six bytes —
`stable` and `1.95.0` are the same rustc, and `rustc --version` says so for both. It matters
because a component's bytes are a `ruleset_hash` input: two people on one commit with one compiler
get two cache keys if one of them reached it by a different name. `rust-toolchain.toml` makes this
a non-issue inside the repository and does not reach a fixture copied out of it.

**wasmtime's import list counts resource types, so `imports().len() == 1` rejects every rule.**
`wasm-tools component wit` shows one import on a component built against `lanekeep:host@0.1.0`,
and `Component::component_type().imports()` shows three for the same bytes: that instance, plus
bare `check-context` and `reduce-context` type imports the component model requires because those
types appear in an export signature. Filter on `ComponentItem::ComponentInstance` — the instances
are what describe reachable capability, and the type imports are bookkeeping.

**An imported resource with no `with` mapping compiles to an uninhabited type.** `bindgen!` emits
one for every host-implemented resource the embedder has not named a Rust type for — `match x {}`
on it compiles — so nothing can be pushed into a `ResourceTable` and no export taking
`borrow<check-context>` can be called. Everything still builds and the failure is only visible
when something tries to run. The `with` key is `package/interface@version.resource`, with a *dot*
before the resource name; a slash there fails with "interfaces were specified in the `with` config
option but are not referenced in the target world", which reads like the interface name is wrong.

**A WIT world has no optional exports, so widening it breaks every existing guest that already
targets it, not only guests written afterward.** Adding `export metadata: func() -> rule-metadata;`
to `world rule` made `cargo component build` refuse every one of the ten committed fixtures that
already had an `impl Guest for Component` block, all with the same message: ``error[E0046]: not
all trait items implemented, missing: `metadata` ``. `wit-bindgen` generates a new export as a
required trait method with no default body, so the fix is a stub `metadata()` in each fixture —
mechanical, but not visible from the world change alone, only from actually building a fixture
against it. `spike` is the one committed fixture this does not reach, because it targets its own
`wit/spike.wit` rather than the shared world. Budget for the identical fan-out the next time an
export is added — `configure` will hit every one of the same fixtures for the same reason.

**A reference's own `Clone` impl is reached only when the pointee has none — this reads backwards
on first sight, and it is worth stating the right way round rather than the intuitive-sounding
wrong one.** `&T: Clone` holds unconditionally (`impl<T: ?Sized> Clone for &T`), but when `T`
itself is also `Clone`, method resolution still reaches through the reference and calls `T`'s own
impl — that is the ordinary case, relied on everywhere `x.clone()` is written on a `&T` to get an
owned `T`. It is only when `T` has *no* `Clone` of its own that resolution falls back to the
reference's, silently handing back the same reference. Confirmed with a two-line probe: `(&NoClone
{}).clone()` compiles to a no-op with `#[warn(noop_method_call)]` naming the exact cause ("the type
`NoClone` does not implement `Clone`, so calling `clone` on `&NoClone` copies the reference"); the
identical call on `&YesClone` where `YesClone: Clone` returns an owned value with no warning at
all. `self.rule(slot)?.clone()`, where `rule` returns `Result<&Rule, WasmError>`, hit the silent
case — `Rule` (`bindgen!`'s generated struct) implements neither `Clone` nor `Copy`.

**And chaining it into a second use is what turns a silent no-op into a diagnostic that names the
wrong cause.** `let n = self.get()?.clone(); self.use_it(&n)` — reproduced standalone, same shape —
fails with `E0499: cannot borrow *self as mutable more than once at a time` and prints no
`noop_method_call` warning at all, even though the identical `.clone()` on its own does. So the one
diagnostic that appears blames a lifetime, and the lint that would have named the real cause never
fires in this shape. `(*self.rule(slot)?).clone()` or `Clone::clone(self.rule(slot)?)` would have
at least traded `E0499` for `E0599: no method named `clone` found for struct `Rule``, a diagnostic
that points at the actual problem — not a working clone, since `Rule` has none to reach. The fix
that was actually right here was to not clone at all and reuse the `with_instance` pattern
`has_check`/`has_reduce` already established.

**A wasm trap's rendered error already contains the method name, so asserting on it proves
nothing.** wasmtime prefixes a host function's error with a backtrace whose top frame is spelled
`wit-component:shim!indirect-lanekeep:host/types@0.1.0-[method]check-context.today`. A test doing
`format!("{err:?}").contains("check-context.today")` therefore passes whatever the host said — it
survived a mutation that made the host name a different method entirely, which is how it was
found. Assert on `err.root_cause().to_string()`, which is the host's own message and nothing else.

**`wasm32-unknown-unknown` has no atomics target feature, so an `AtomicU64` is a plain load and
store and a busy loop written around one is deleted.** A fixture that has to spend real time —
the only kind that can test a wall-clock budget, per the interrupt-handler trap above — cannot
rely on either of the two obvious guards. Storing the accumulator into a `static AtomicU64` once
at the end lets LLVM strength-reduce a linear congruential step to a closed form and drop the
loop; storing into it on *every* iteration does not help either, because without the atomics
feature `core::sync::atomic` lowers to ordinary memory operations and every store but the last is
removed. Both were measured: a 20 ms budget failed to notice four hundred million rounds of each,
and the test passed in under a second. `core::hint::black_box` inside the loop is what makes it
real — `crates/lanekeep-wasm/tests/fixtures/limits/` already used it, and
`.../fixtures/engine-rule/` says why in its `burn`.

**A trap poisons a `wasmtime::Store` for good, and the store outlives the file.**
`bindgen!`'s `imports: { default: trappable }` means any trap sets a store-wide flag with no
public reset, so the next call on that store fails with `cannot enter component instance` — a
message about the runtime's bookkeeping that names nothing that went wrong. rayon keeps handing
a worker its remaining files after one fails, and which of several simultaneous failures the
reduction surfaces is arbitrary, so a run can be reported against a file that was fine. Nothing
is rescued by noticing, since every such failure cancels the run either way; the *diagnostic* is.
`lanekeep-engine`'s `Worker::poison_on` remembers the first failure and hands it back for the
rest of that worker's share.

**`git log -- <a committed binary>` lists the commits where its bytes changed, and a rebuild
that produces identical bytes is not one of them.** So "the source commit is newer than the
artifact commit" is not evidence that the artifact is stale — it is equally the signature of a
rebuild that changed nothing, and the two are indistinguishable from history alone. Three of the
eleven committed WebAssembly fixtures read as stale that way — `bindings`, `spike` and
`world-shape` — and all eleven turned out to be current, which only a rebuild could establish:
`cargo component build --release` on the pinned toolchain is byte-reproducible, so
`just wasm-fixtures` on a consistent tree leaves `git status` clean and on an inconsistent one
does not. That is now the check rather than the investigation —
`crates/lanekeep-wasm/tests/fixture-digests.txt` records what every artifact was built from, and
`tests/fixture_currency.rs` fails when the sources beside it have moved. `wit/world.wit` is in
there too, because ten of the eleven name it as their component target and a world edit with no
rebuild leaves every fixture satisfying an ABI that no longer exists.

**A file watcher over the project root sees lanekeep's own cache writes.** `.lanekeep/` lives
inside the root, so a `--watch` loop that reacts to every event re-checks, writes the cache,
and re-checks forever — pinning a core while the output looks exactly like a tool that is
working. `crates/lanekeep-cli/src/watch.rs` filters by path *component* rather than substring,
so `target/` is ignored and `src/target.ts` is not.

**release-plz compares against the registry, and the registry lags a gated publish.** It works
out the next version by diffing a package's packaged files against the newest version on
crates.io. Merging a release pull request tags immediately, but nothing is published until
someone approves the `release` environment — so for the length of that approval the registry is
one version behind the manifest. Any push to `main` in that window finds the packaged
`Cargo.lock` differs and proposes *another* release: after v0.3.2 it opened one for 0.3.3 whose
whole changelog was "update Cargo.lock dependencies". Merging it would have published a version
identical to the one still waiting to go out, and no index lets a number be reused.

The fix is to gate the window itself: `release-plz.yml` reads the workspace version from
`Cargo.toml`, asks crates.io whether it is published, and skips the release-pull-request step
when it is not. Tagging stays ungated, or a merged release pull request would never tag.
Comparing the manifest rather than the newest tag is load-bearing — right after a release pull
request merges the tag does not exist yet, so a tag-based check reads the previous, published
one and lets the failure through.

**The first attempt was `release_commits`, and it was a proxy rather than the condition.**
Filtering to `feat`, `fix`, `perf` and `revert` closes the case where the only commit in the
window is a `chore: release` — which was the first failure's shape, so it looked right. Then a
real `feat` landed in the window, matched the filter, and proposed a duplicate of a version
already publishing. Worth remembering generally: when a fix works by excluding the example you
have rather than by describing the fault, expect the next instance to walk straight past it.

Not `git_only` either, which reads versions from tags and would strand the fourteen crates this
repository deliberately leaves untagged.

The mirror-image trap is worth holding at the same time: **a change that ships different bytes
without touching crate source proposes nothing at all.** The glibc fix lived entirely in build
tooling, so release-plz saw no package change while every binary it shipped was different. That
one needs a version bump by hand; `docs/releasing.md` has the steps.

**`gates.fileContains` is an *and*, not an *or*.** Every listed substring has to be present,
so a rule matching either of two tokens — `unwrap` or `expect` — cannot express its gate as
`['unwrap', 'expect']`. That rejects any file containing only one of them, which is nearly all
of them. Nothing fails: the rule loads, the query never runs, and the output reads exactly like
a codebase with none of the thing in it. There is no *or* form, so a rule with no single
covering substring omits the gate rather than writing one that is wrong.

**A rule that declares `check(ctx, m, options)` and exports a plain object silently ignores every
option it documents.** A handler is invoked with two arguments — `...rules[i].check(ctx, {...})` —
so a third parameter is always `undefined`. Options reach a rule only by being closed over, which
means the default export has to be a *factory*. `no-unwrap` and `no-glob-import` both declared the
parameter, both documented an `allow` option in their own JSDoc and in `docs/built-in-rules.md`,
and both exported `defineRule({...})` directly, so `allow` did nothing from the day each shipped.

Three things hid it, and the third is the one worth carrying forward. No test configured either
rule — `RuleTester::configured` existed but had no `with_extension` variant, so no non-TypeScript
rule could be tested with options at all. The failure is invisible in the passing direction: an
ignored `allow` only ever *adds* violations, and a user who sees one assumes their pattern is
wrong. And `packages/lanekeep/builtin.d.ts` declares a built-in as `Rule & ((options?) => Rule)` —
a superset covering both shapes deliberately, because the specifier cannot say which one a rule is
— so TypeScript accepts the call that throws `not a function` at run time.

Both shapes have to keep working, which is why the fix is neither "make it a factory" nor "leave
it an object": `lanekeep init` writes a bare `"lanekeep/no-unwrap"` into a Rust project's config
and `lanekeep-config` renders that as the imported binding itself, while the documented usage
calls it. The rule is now a factory whose properties are copied onto the function
(`for...in`, not `Object.assign` — the sandbox's intrinsics are an allowlist and `Object` is not
on it), which is what `builtin.d.ts` claimed all along. **The three genuine factories —
`no-restricted-imports`, `no-circular-imports`, `no-unused-exports` — are the mirror image and are
not fixed: referenced bare from a JSON config they render as a function where a rule object is
expected.** No scaffold emits one, so nothing trips it today.

**`Sandbox::eval_module` does not go through the loader, so the synthetic entry module is not in
`ruleset_hash`.** `hash_ruleset` folds over what `RuleLoader` recorded, and the loader only sees a
module something *imported*; the entry is handed straight to `Module::evaluate`. Everything the
entry module carries and nothing else carries is therefore outside the cache key. That was exactly
a `lanekeep.json` rule's `options`, which were interpolated into the entry as a factory-call
argument and appeared in no hash at all: editing `{"rule": "x", "options": {"limit": 1}}` to
`{"limit": 2}` produced two byte-identical `config_hash`es and a warm run kept answering the
previous configuration. `docs/architecture.md` §8.1 has listed options under `config_hash` the
whole time, so the code and the specification disagreed and neither said so. Options are read as
data and hashed now, on the path that knows them — but the general fact stands: **a value that
only ever exists in generated entry-module source is invisible to both hashes.**

**What hid it is worth more than the mechanism: a fixture written against a `lanekeep.config.ts`
passes against this bug.** The two config formats reach the key by different routes — a TypeScript
config's options live in the config module's own source, which the loader *did* read, so they were
hashed all along — and every hashing test there was covered one format. Single-format coverage of a
property both formats have to satisfy is not coverage of the property. When two paths can reach the
same requirement, assert it on each of them, in matched pairs, and name the pairing so nobody drops
half of it later.

**A fixture path derived from its content races, and the *repair* for the obvious version of that
bug races too.** `json.rs`'s test helper first keyed `temp_dir()/lanekeep-json-…` on the config's
*length*: two thirty-eight-byte configs shared one file, tests run in parallel, and each read
whichever had been written last. Keying on a `blake3` of the content looks like the fix and is not
— two tests can legitimately write the *identical* config, and `std::fs::write` truncates before it
writes, so the sibling thread reads an empty file and fails with `EOF while parsing a value at line
1 column 0`. Measured five failures in eighty runs of `cargo test -p lanekeep-config`; **same bytes
is not the same as no race, because truncate-then-write is not atomic.** Name the directory after
the *test*, or write to a temporary and rename. Nothing derived is safe here, and the derivation
that is nearly safe is the one that costs the most to disbelieve.

Both versions surfaced during mutation testing, where a mutation of the hashing code was reported as
breaking a specifier-parsing assertion — worse than an ordinary flake, because the whole point of
that exercise is trusting the attribution. `just check` hid it: nextest runs a process per test,
which widens the window enough that a hundred runs were clean, while the plain `cargo test` the
brief prescribes was failing one run in sixteen.

**A node handle is an integer and the root's is `0`, so `if (!node)` discards it.** Nodes cross
into the sandbox as handles rather than objects — one of the one-way doors in §14 — and the
root is handle zero. A rule written the ordinary JavaScript way, `const parent = ctx.parent(n);
if (!parent) return`, therefore treats every top-level item as parentless. `no-unwrap` lost its
whole `#[test]` exemption to this, silently, because the check it skipped only ever *removed*
violations. Compare against `undefined` explicitly, or read `ctx.ancestors` positionally and
avoid the question.

**Validating a flag is not applying it, and validation is what makes an ignored flag look
implemented.** `check` destructured `--timeout`, rejected zero with a considered message — whose
comment warned that accepting a value and ignoring it "would surface much later as an
unexplained breach of a budget they thought they had changed" — and then called `prepare`, which
had no parameter for it. The value was dropped one line below the comment explaining why that
must not happen. It survived because the failure has no symptom of its own: lowering a budget
looks like it worked, since the run completes either way, and raising one looks like the run is
simply slow. The breach message even ends with "raise it with `--timeout`", advice printed by
the code that made it impossible. A test that only *lowers* a limit passes against this bug —
assert the raise.

**Both engines poll the global run budget only from inside a handler, so for a while it bounded
a run only while a handler was executing.** QuickJS polls it from its interrupt handler; wasmtime
from the epoch checks Cranelift compiles into guest code. Neither runs while the engine is
reading, hashing, parsing or matching, and §15's cold cost is dominated by exactly that. So a
rule whose handler returned after a handful of operations could overrun the budget without ever
being asked to stop: 400 files against a one-line rule ran to completion under a 1 ms budget,
config-set or flag-set alike, and the component path had the same gap for the same reason. It was
never that the limit is unwired — a rule doing real work trips it precisely.

`Engine::check_file` now asks the run clock between one file and the next, which closes it for
both engines at once because it sits above the dispatch that chooses between them. Two things
follow. **A fixture for the *inner* limits still needs a rule that burns real bytecode**, or it
passes because the work was fast; a fixture for the *outer* check needs the opposite — a handler
so cheap that nothing but the outer check could have stopped the run, or it passes against the
bug. And **an aborted run has to commit the entries for files that completed, and must not
prune.** Those two halves have different histories and it is worth keeping them apart. The
*commit* half architecture §6.8 always required and nothing did: `run_files` returned on the
first error before it reached the save, so a corpus that overran its budget would have been
stranded cold forever the moment the budget started being enforced. The *no-prune* half is
doctrine this change added to §6.8, and it could not have been there before — an aborted run
wrote nothing at all, so there was no save whose pruning behavior anyone had to decide.

**A Linux binary's glibc floor is inherited from the runner image unless something pins it.**
A dynamically linked binary cannot run against a glibc older than the one it was built against,
so the build machine silently decides the oldest distribution the release supports. When
`ubuntu-latest` rolled from 22.04 to 24.04, lanekeep's floor went 2.35 → 2.39 and v0.3.1's
Linux binary stopped starting on Ubuntu 22.04, Debian 12 and RHEL 9 — on npm, on the releases
page and in Homebrew at once. Nothing went red, and nothing would have: the smoke test runs on
the machine that built the binary, which is the one machine where the floor is never wrong.

Two symbols did it, `pidfd_getpid` and `pidfd_spawnp`, pulled in by Rust's std rather than by
anything here. Chasing individual symbols is the wrong fix; stating the floor is the right one.
Linux targets now build with `cargo zigbuild --target <triple>.2.17`, and
`scripts/check_glibc_floor.py` parses the ELF's `.gnu.version_r` and fails if the result needs
more than it claims. Nothing but a wheel's `manylinux` tag ever forced the number to be written
down, which is why this surfaced with the PyPI lane and not before.

**Python's stdout on Windows is cp1252, not UTF-8, and this repository's prose is full of em
dashes.** Distinct from the CRLF trap above and with a different symptom: `sys.stdout.write` of
any text carrying one dies with `UnicodeEncodeError` partway through, so the output is
*truncated at the first non-ASCII character* rather than mangled. A helper that read a wheel's
METADATA — which embeds the README — passed everywhere but Windows, where four assertions failed
because the text simply stopped. `sys.stdout.buffer.write(...)` of the raw bytes is the fix, and
it avoids the newline translation as well. Reading is already safe as long as every
`read_text`/`open` names `encoding="utf-8"`, which they must.

**A shell stub that pipes a command through `sed` reports `sed`'s exit status.** The CRLF
simulation in `test-shell-portability.sh` wrapped `python3` that way, so every invocation
appeared to succeed regardless of what it did. Any script whose control flow turns on python's
exit code could fail all of its cases there and still be reported as tolerating CRLF. `set -o
pipefail` in the stub is the fix. Worth remembering generally: a stub is test code, and test
code that always passes is worse than none.

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

**When an encoding changes, a test pinned to it needs its data re-derived, not merely re-run.**
`two_components_cannot_run_together_into_one` (`lanekeep-config`) proved two components could
not concatenate into one cache key by constructing a real collision under the *old* encoding:
`hash_ruleset` used to write a `\x01` presence marker before each component's bytes, with no
length prefix, so `a.wasm = "AA"`, `b.wasm = "BB\x01CC"` and `a.wasm = "AA\x01BB"`, `b.wasm =
"CC"` produced the identical nine bytes `\x01AA\x01BB\x01CC` either way — the marker embedded in
the content played the real marker's own role at the split point. Removing the marker
(`hash_ruleset` now only length-prefixes) left that data proving nothing: with no marker to
fake, the same two rows differ even with the length prefix hypothetically gone, so
`length_prefixed` could be deleted from the component fold and this test stayed green — the
code under test never changed, only what the data meant. The change that invalidated it sat
three hundred lines away in the same commit. The fix rederived the data — `"AA" + "BBCC"`
against `"AABB" + "CC"`, which really do collide once concatenated without a length — rather
than trusting "still passes" to mean "still tests," and shipped in `e85521d`: a reader who goes
looking for the corresponding change in whichever commit this paragraph sits in will not find
it there.

**"A mutation of the code under test cannot reveal a stale fixture" is not the lesson, and is
not even true.** Two mutation-testing runs against `hash_ruleset` missed this gap, but not
because mutating the code under test categorically cannot expose stale data under it —
`cargo-mutants` mutates by replacing a *whole function body*, so `length_prefixed with ()` guts
every caller at once, and `hash_config` calls `length_prefixed` too and has several tests of its
own. That mutant dies there regardless of what `two_components_cannot_run_together_into_one`
does. The real mechanism is narrower and more worth knowing: a mutation run reporting a shared
helper as "caught" does not say *which* caller's test caught it, so `hash_config`'s coverage
stood in for `hash_ruleset`'s missing coverage without either number saying so — masking
exactly the gap it looked like it was closing. A fixture is a claim about the encoding it was
built against, and it goes stale exactly like documentation does: silently, and without whatever
broke it saying so. Checking whether a mutation run actually exercises the code path you think
it does means checking which test failed, not only that one did.

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
