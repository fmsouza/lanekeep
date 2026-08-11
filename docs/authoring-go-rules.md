# Authoring a rule in Go

A rule can be a TypeScript module or a WebAssembly component. This is how to write the second
kind, in Go, in [`go-rules/`](../go-rules). Two built-ins are written this way today —
`lanekeep/no-context-in-struct` and `lanekeep/no-package-init`, the two that check Go — and both
ship inside one artifact, `crates/lanekeep-rules/components/go-builtins.wasm`.

Read [`architecture.md`](architecture.md) §4 and §6.9 first, and
[`authoring-rust-rules.md`](authoring-rust-rules.md) if you have not written a component rule
before — everything it says about the world, the ids, the options and the two shipping tables is
true here and is not repeated. This file is the mechanics of the Go lane, and the six things
below each cost real time to find.

## Six things that are not preferences

Two of them stop a build outright and say so. The other four ship an artifact that is valid,
loads, and passes an import assertion — while giving every checkout a different cache key,
discarding every report the rule made, or reporting the wrong thing. None is discoverable from
the message it produces.

**The build command is exact, and `-no-debug` is correctness rather than size.**

```sh
tinygo build -target=wasm-unknown \
  -wit-package <repo>/crates/lanekeep-wasm/wit -wit-world rule \
  -panic=trap -no-debug -o <out>.wasm .
```

Without `-no-debug`, TinyGo writes the build directory's path into the artifact's DWARF. A
component's bytes are a `ruleset_hash` input (architecture §8.1), so identical source built in
two checkouts gives two cache keys — which is the rustup-toolchain-name trap AGENTS.md already
records, and strictly worse: a toolchain name differs between two people who reached one compiler
by different routes, and a checkout path differs for everybody. Measured on TinyGo 0.41.1 against
`go-builtins.wasm` at `2ce7aef`: **13,187 bytes** with the flag and **322,301** without, and
`strings … | grep -cE '/Users|/opt/homebrew'` finds **0** lines against **85**, twelve of which
name the worktree it was built in. Do not drop it while debugging. `just go-rules` passes it;
do not build these by hand.

**`wasip2` cannot build against this world at all**, and the refusal is at encode time rather
than at load:

```
error: failed to encode a component from module
Caused by:
    0: failed to decode world from module
    1: module was not valid
    2: failed to resolve import `wasi:cli/environment@0.2.0::get-environment`
    3: module requires an import interface named `wasi:cli/environment@0.2.0`
```

`world rule` declares one import and TinyGo's wasip2 runtime needs `wasi:cli/environment`, so
`wasm-tools component new` refuses the module. No flag combination fixes it, and that is better
news than a component lanekeep would reject at load: the failure is loud, local, and impossible
to ship past.

**The SDK resets TinyGo's map-iteration generator for you, and you cannot opt out.** TinyGo
randomizes map iteration from a package-level pseudo-random generator that *advances*, and the
host instantiates once per (worker, component) — so without a reset, a rule's map order is a
function of how many files that worker already handled, which is a rayon scheduling artifact.
`lanekeep.NewHandlers` is the only constructor for the type the component entry dispatches
through, and its four methods reset before delegating. See [below](#the-determinism-hazard-you-cannot-see)
for why it is enforcement rather than a note in a doc comment.

**`cm.Option[T].Some()` is a pointer method**, so `ctx.Text(n).Some()` does not compile —
`cannot call pointer method Some on cm.Option[string]`. Bind the option to a variable first.
`Value()` chains and is what every string comparison uses, which is exactly what makes the next
one dangerous.

**`import` is the zero value of `binding-kind`, and `Value()` hands back the zero value for a
`none`.** So `ctx.BindingKind(n).Value() == types.BindingKindImport` is true for a name that
resolves to no binding at all, and the rule reports every unresolved qualifier. It only ever
*adds* violations, so it reads as a strict rule rather than a broken one. Write:

```go
kind := ctx.BindingKind(pkg)
binding := kind.Some()
if binding == nil || *binding != types.BindingKindImport {
	return nil
}
```

**A `borrow<>` needs an explicit `defer …ResourceDrop()`.** `wit-bindgen-go` 0.7.0 emits none, so
a guest that simply uses the borrowed context returns with the loan outstanding and wasmtime
answers `borrow handles still remain at the end of the call` — after the rule body has run, so
**every report the rule made is discarded**. `go-rules/main.go` carries the drop for the two
exports that take a context; a rule never sees this, and an out-of-tree author writing their own
entry does.

## When to reach for it

Same answer as Rust. A component earns its place when the rule is *about* a language and the
person maintaining it would rather write that language. The host API, the query gate, the limits,
the ids, the options and the output are identical, and there is no capability a component has
that a TypeScript rule lacks.

What Go costs beyond that: a build step no gate runs (the artifact is committed), options that
must survive JSON, and a toolchain — TinyGo and Go — that `just check` deliberately does not
require and `just setup` does not install.

## The shape of the module

`go-rules/` is one Go module, `github.com/fmsouza/lanekeep/go-rules`, and it is **not** a member
of any Cargo workspace or of the repository's root Go module. That last part has a consequence
worth knowing before you add a package here: `./...` is scoped to a module rather than to a
directory, so a wildcard run from the repository root reaches `cmd/lanekeep` and nothing under
`go-rules/`. `just test-go` therefore runs `gofmt -l`, `go vet` and `go test` in **both** modules
— it did not always, and for three commits the SDK's tests ran in `just go-rules` and nowhere
else, which is to say in no gate at all.

```
go-rules/
  go.mod  go.sum          module path, and `cm` pinned to v0.3.0
  internal/lanekeep/host/ the generated bindings, committed
  lanekeep/               the SDK
  main.go                 the component entry: the rule table and the seven exports
  rules/<name>/rule.go    one package per rule
  fixtures/maporder/      a component that is not a rule; see the determinism section
```

**The bindings are generated once and committed**, which is what keeps `just check` free of a Go
toolchain entirely. They are deterministic — regeneration is byte-identical — and they are
generated against the engine's own `wit/` rather than a copy, so there is no second world to
drift:

```sh
cd go-rules
wit-bindgen-go generate --world rule --out ./internal \
  --package-root github.com/fmsouza/lanekeep/go-rules/internal \
  ../crates/lanekeep-wasm/wit/world.wit
```

Two pins that are not preferences. **`go.bytecodealliance.org/cm` is v0.3.0**, because the v0.7.0
tag of that repository declares the parent module path and `go get …/cm@latest` fails with
`module declares its path as: go.bytecodealliance.org but was required as:
go.bytecodealliance.org/cm`. And **`go 1.23.0`** in `go.mod` rather than whatever `go mod init`
wrote, because the floor that actually binds is `cm`'s own `go 1.22.0` and declaring a newer one
refuses the module to a contributor on an older toolchain for no reason.

## What the component exports

The `rule` world's exports, which [`world.wit`](../crates/lanekeep-wasm/wit/world.wit) is the
authority on: `rules`, `metadata`, `configure`, `has-check`, `has-reduce`, `check`, and `reduce`
when the rule has a cross-file pass. A WIT world has no optional exports, so adding one to the
world costs a stub in every guest that targets it — which is why the component entry, and not
each rule, is where they are written.

**You do not write the dispatch.** [`go-rules/main.go`](../go-rules/main.go) is the Go
counterpart of `rust-rules/lanekeep-rule`'s `ruleset!` macro, and it is a plain slice rather
than a macro because Go's generated bindings are an ordinary importable package — the SDK can
name the types in a handler's signature, so nothing has to be expanded. A rule joins `ruleset`:

```go
var ruleset = []hosted{
	{id: nocontextinstruct.ID, Handlers: nocontextinstruct.Handlers()},
	{id: nopackageinit.ID, Handlers: nopackageinit.Handlers()},
}
```

**Ordered by id, and that is a constraint.** `crates/lanekeep-rules`' `COMPONENT_RULES` is sorted
by rule name and carries the index this component is dispatched on, so a rule inserted in the
middle renumbers every rule after it and those rows move in the same change. A table in any other
order means a config naming one rule running the other, with both sides answering perfectly well
and nothing anywhere to notice. `crates/lanekeep-wasm/tests/world_shape.rs` asserts the order
`rules` reports.

A rule package exports two things: an `ID` constant and a `Handlers()` returning
`lanekeep.Handlers`, built by the SDK's only constructor:

```go
func Handlers() lanekeep.Handlers[types.RuleMetadata, types.CheckContext, types.ReduceContext] {
	return lanekeep.NewHandlers[types.RuleMetadata, types.CheckContext, types.ReduceContext](
		Metadata, nil, check, nil,
	)
}
```

The four arguments are `metadata`, `configure`, `check` and `reduce`. Only `metadata` is
required — the host reads it at prepare time, and a rule without it cannot load. Pass `nil` for a
pass the rule does not have and `has-check`/`has-reduce` answer from the same value the dispatch
uses, so what the component says about itself and what it does cannot disagree. All three type
parameters are named at the call site because an untyped `nil` carries nothing to infer from.

`metadata` is the rule's id, languages, severity, card, query, gates and timeout — the
component's answer to what a `defineRule` call carries, validated by exactly the code that
validates a TypeScript rule's. `configure` is handed the rule's options as a JSON string,
`"null"` for a rule named with no options; parse it with `encoding/json`, and return an error to
*refuse* a configuration rather than to fail. `check` and `reduce` return a plain Go `error`,
which the entry converts into the world's `result<_, rule-error>`.

[`go-rules/lanekeep`](../go-rules/lanekeep) is the SDK, and it is four things:
`Capture(m, "name") (Node, bool)`, `GlobMatches(pattern, value)`, `ResetRand()`, and the
`cabi_realloc` export. The first two mirror `rust-rules/lanekeep-rule`; the last two are
TinyGo's tax and exist so no rule author meets either as an error message.

## Four more things that will bite

**A node handle is an integer and the root's is zero.** `Capture` returns `(Node, bool)` and the
`bool` is the answer — `(0, true)` is the root, not a miss. This is the `if (!node)` bug that
cost `no-unwrap` its whole `#[test]` exemption, and it is silent for the same reason: skipping a
check only ever removes violations.

**Every list a rule hands the host is a package-level variable, not a local.** `cm.ToList` does
not copy: it takes the slice's data pointer and length, and the host reads through that pointer
after the export has returned. A slice literal inside `Metadata` is a local whose escape TinyGo
would have to infer through `unsafe.SliceData`, and a wrong inference there is a lifted list
pointing at reclaimed stack — the one shape that can dangle on a target whose collector never
frees anything.

**Nothing in a rule may panic.** `-panic=trap` turns a Go panic into a wasm trap with no payload
at all: no message, no stack, and the trap aborts the call before any report crosses back. So a
wrong assumption about a match's shape surfaces as "the host recorded no violations", which is
indistinguishable from a rule that legitimately found nothing. Every lookup answers rather than
indexes, and `check` returns `nil` rather than erroring on a shape it does not recognize.

**A component's globals are shared by every rule it hosts**, since the host instantiates once per
(worker, component). Anything outliving a `check` call must be derivable from that call's
inputs — a memo keyed on the file path qualifies; a counter of files seen does not. Only
`configure`'s options are per rule index. This is the same rule `authoring-rust-rules.md` states
at length, and in Go it has a second edge, below.

## The determinism hazard you cannot see

TinyGo randomizes map iteration. `src/runtime/hashmap.go` seeds a map's hash seed and an
iterator's start bucket and start index from `fastrand()`, a xorshift over a single package-level
global in `src/runtime/algorithm.go`.

On `-target=wasm-unknown` there is no entropy to seed it with — `hardwareRand()` is
`return 0, false` — so the sequence is fixed and three separate processes produce byte-identical
map orders. That sounds like the end of it and is not, because iteration order depends on the
*position* in the sequence rather than on the seed. Measured with TinyGo 0.41.1, varying only how
many draws happened first:

| Prior draws | Resulting map order |
| --- | --- |
| 0 | `[3809771, 16096528, 16096528, 13700953, …]` |
| 1 | `[3809771, 13700953, 16096528, 16096528, …]` |
| 4 | `[13700953, 16096528, 16096528, 6767677, …]` |
| 13 | `[6767677, 13700953, 8944334, 13700953, …]` |

Put that beside the instance lifetime the world already fixes — one instance per (worker,
component), shared across every rule it hosts and every file that worker handles — and the number
of draws standing before any given `check` is a function of rayon's work-stealing schedule. **A
Go rule that iterates a map produces scheduling-dependent output, on a fixed seed, with every
cache-key input identical.** Sorting violations by `(ruleId, file, line, column)` does not rescue
it: a rule that picks *which* node to report by map order reports a different violation, not the
same one in a different position.

No rule author can see this, because the state at fault is inside TinyGo's runtime rather than in
the rule's source. So it is closed by construction rather than by documentation:
`lanekeep.Handlers`' fields are unexported, `NewHandlers` is the only constructor, and all four
methods call `ResetRand()` before delegating. `metadata` and `configure` are covered as well as
the two passes — a `configure` that decodes options into a `map[string]any` and ranges it is the
ordinary way to write one.

An earlier version of this SDK asked authors to remember the call. That is documentation standing
in for enforcement of a hazard whose failure mode is a rule passing every test anyone would write
and misbehaving only under a real corpus. `ResetRand` stays exported only because the fixture
that proves the reset works has to drive it directly.

`crates/lanekeep-wasm/tests/go_map_order.rs` is that proof: it drives
`go-rules/fixtures/maporder/` — a component that is not a rule and ships to nobody — six times
through one production `WasmRuntime` instance, asserting one distinct message and
`instantiations() == 1`. Its sibling test asserts the fixture's order *moves* with the generator
position, because a fixture whose observable is constant would make the first test green over a
`ResetRand` that does nothing.

## Building, testing, committing

```sh
just go-rules
```

That runs `gofmt -l`, `go vet ./...` and `go test ./...` inside `go-rules/`, builds
`go-builtins.wasm` into `crates/lanekeep-rules/components/`, builds the map-order fixture into
`crates/lanekeep-wasm/tests/fixtures/`, and records what each was built from in
[`crates/lanekeep-wasm/tests/go-component-digests.txt`](../crates/lanekeep-wasm/tests/go-component-digests.txt).

It needs TinyGo and Go; `just setup` installs neither, so `just _require tinygo` will tell you to
run a recipe that does not help. Install TinyGo yourself. **The gate needs neither** — the
artifacts are committed, and `fixture_currency.rs` reddens when a source under `go-rules/` moves
and the artifact does not. That test is what makes running this recipe non-optional rather than a
convention.

The digest manifest records `go-rules/` **wholesale**, tests included, and
`crates/lanekeep-wasm/wit/` as a directory. So editing `glob_test.go` demands a rebuild. That is
deliberate: a hand-maintained list of the non-test files is a list that silently stops covering a
new one, and over-invalidation costs a rebuild that is cheap and reproducible.

**The build is byte-reproducible**, unlike `componentize-js`'s, so AGENTS.md's
rebuild-and-see-`git status`-clean protocol works here: two builds from an unchanged tree, one
through the recipe and one through its `tinygo` line by hand, both gave sha256
`881bc3cc47da05a6e76e59067a97581762bb53a1461f40d807f167b67d7d4a3c` on TinyGo 0.41.1. The digest
manifest is a second opinion rather than the only check.

`go build ./...` is deliberately not in the recipe and will fail if you run it. `main.go` is a
`package main` whose imports are `//go:wasmimport` declarations with no body, so it type-checks
everywhere and *links* only on a wasm target; on the host it dies with `relocation target …
not defined`. `go vet` and `go test` compile every package without linking that one.

Test the rule in `crates/lanekeep-rules/tests/<name>.rs` with **`RuleTester::for_built_in`**,
which names the rule by specifier — `"lanekeep/no-package-init"` — and resolves through the
embedded table to one rule of the shared component. `RuleTester::for_component` writes the
artifact to a path, and a path reference contributes *every* rule the artifact hosts, so a second
Go rule can report into the first one's expectations.
[`crates/lanekeep-rules/tests/no_package_init.rs`](../crates/lanekeep-rules/tests/no_package_init.rs)
is the worked example.

A rule package itself has no Go unit tests, and cannot: `check` takes a `types.CheckContext`,
a resource handle whose methods are `//go:wasmimport` declarations, so it is not callable on the
host at all. The coverage is `crates/lanekeep-wasm/tests/world_shape.rs`, which drives the
committed artifact through a stub host, plus the `RuleTester` suite above, which drives it
through the real engine over real Go source.

**A stub host that always answers `Some(…)` cannot catch the `binding-kind` trap.** The `None`
arm in `world_shape.rs`'s stub is the only reason a committed test sees it.

## Shipping it as a built-in

Two tables in [`crates/lanekeep-rules/src/lib.rs`](../crates/lanekeep-rules/src/lib.rs), both
kept in order, exactly as for Rust:

- `BUILT_IN_COMPONENTS` — the component. `go-builtins` has one row however many rules it hosts.
- `COMPONENT_RULES` — `(rule name, component name, index)`, one row per rule. This is what a
  config resolves through, and the index must match the position in `go-rules/main.go`'s
  `ruleset`.

A rule name belongs to exactly one of `BUILT_IN_RULES` and `COMPONENT_RULES`; a name in both
would be two programs answering to one id. There is **no** `COMPONENT_SOURCES` entry — that table
is the TypeScript `just typescript-builtins` compiles, and a Go rule has none.

Add both rows in one change. A component listed only in the first table is bytes embedded in
every shipped binary and reachable by no specifier, and `every_component_is_a_rule_that_ships`
fails the gate in both directions.

## Migrating an existing rule

Write the expectation table first, exactly as `authoring-rust-rules.md` describes: turn the
existing test file into a table of cases and a function that runs the whole table against a
tester it is handed, run it against the TypeScript arm, and commit that alone. Then add the
component arm and watch it fail. Keep both arms until the swap lands, then **delete the
TypeScript source and its arm in one commit**.

Deleting the source is not tidiness. A rule that is a component *and* keeps a source resolves
through the source when its `COMPONENT_RULES` row is missing — for a TypeScript rule that is one
program on a second engine and harmless, and for a Go rule it is a different implementation in a
different language answering to the same id, with nothing in the output to say which one
reported. With the source gone, the same mistake is an error.

Two things that survived the two migrations here and are worth copying. Compare every string in
`metadata` against the original **mechanically** rather than by eye — a script that pulls the JS
literals out, decodes their escapes and compares them against what the committed component
answers through the ABI, with a control that capitalizes one character and watches the comparison
fail. And keep a deliberately preserved false positive as a case rather than fixing it in
passing: `a_different_package_aliased_to_context_is_also_reported` is the only place that would
have shown a port quietly changing what a rule means.

## Authoring a Go rule outside this repository

Nothing stops it, and two things are different.

`go-rules/internal/` is not importable from another module, so an out-of-tree author generates
their own bindings with the command above. Their `types.MatchEntry` is then a different Go type
from this module's, so `lanekeep.Capture` will not typecheck against it — copy the eight lines.
`lanekeep.Handlers` *is* usable, because it is generic over the metadata and context types for
exactly this reason.

And the component entry is yours to write, which means the `defer …ResourceDrop()` in `check` and
`reduce` is yours too. [`go-rules/main.go`](../go-rules/main.go) is the file to copy; its doc
comments say why each part is the way it is.
