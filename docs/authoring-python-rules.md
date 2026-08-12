# Authoring a rule in Python

A rule can be a TypeScript module or a WebAssembly component. This is how to write the
second kind, in Python, in [`py-rules/`](../py-rules). No built-in is written this way
today — the two Python-targeting rules cannot ship from `lanekeep-rules` (crates.io's
10 MiB cap) — so `py-rules/` holds them as example rules, and this file is the mechanics
of the lane.

Read [`architecture.md`](architecture.md) §4 and §6.9 first, and
[`authoring-rust-rules.md`](authoring-rust-rules.md) if you have not written a component
rule before — everything it says about the world, the ids, the options and the two
shipping tables is true here and is not repeated. This file is the mechanics of the
Python lane, and the things below each cost real time to find.

## The build command is exact, and `--stub-wasi` is a requirement

```sh
componentize-py -q -d <repo>/crates/lanekeep-wasm/wit -w rule componentize main \
  --stub-wasi -o <out>.wasm
```

A default build imports **26** instances — a wall clock, environment, filesystem and
sockets. A `--stub-wasi` build imports **exactly the declared world**, which is the
confinement the sandbox exists to provide. `just py-rules` passes the flag; do not build
these by hand.

**The artifact is not committed.** `componentize-py` output is not byte-reproducible:
CPython's hash seed is drawn from `wasi:random` during pre-init and frozen into the heap
image, so two builds of one unchanged file differ at millions of byte positions. A
committed artifact would leave a permanently dirty digest line. `just py-rules` builds
it on demand and runs the tests against it; nothing in the gates depends on it existing.

## The shape of the module

```
py-rules/
  lanekeep/               the SDK: Handlers, capture, glob_matches
  rules/<name>/rule.py    one package per rule
  main.py                 the component entry: the rule table and the seven exports
  determinism_probe.py    a component that is not a rule; see the determinism section
  tests/                  the SDK's own tests (stdlib unittest)
```

The SDK is type-agnostic — it never imports the generated `wit_world` bindings — so its
tests run on the host with plain Python. The component entry and the rules import the
bindings, which `componentize-py` generates at build time.

## What the component exports

The `rule` world's seven exports, which [`world.wit`](../crates/lanekeep-wasm/wit/world.wit)
is the authority on: `rules`, `metadata`, `configure`, `has-check`, `has-reduce`,
`check`, and `reduce`. A rule joins the `RULESET` table in `main.py`:

```python
RULESET = [
    (nobroadexcept.ID, nobroadexcept.handlers()),
    (nomutabledefaultargument.ID, nomutabledefaultargument.handlers()),
]
```

**Ordered by id, and that is a constraint.** The index a rule sits at is what every
export but `rules` dispatches on, and a rule inserted in the middle renumbers every rule
after it.

A rule package exports an `ID` constant and a `handlers()` returning
`lanekeep.Handlers`:

```python
def handlers():
    return Handlers(metadata=metadata, check=check)
```

The four arguments are `metadata`, `configure`, `check` and `reduce`. Only `metadata` is
required — the host reads it at prepare time, and a rule without it cannot load. Pass
`None` for a pass the rule does not have, and `has-check`/`has-reduce` answer from the
same values the dispatch uses.

## The traps that are not preferences

**`--stub-wasi` traps rather than absents.** The clock, filesystem, network,
`os.urandom`, `uuid`, `datetime.now()`, `subprocess` and `print()` all trap as a bare
`wasm trap: unreachable` — **uncatchable from Python** (`BaseException` never sees it)
and poisoning the store, so a rule that reaches for one aborts the run. Meanwhile
`random.random()`, `os.environ`, `os.getcwd()` and `sys.version` return frozen values.
A rule must not reach for either half: the first traps, the second is a constant that
looks like an observation.

**A node handle is an integer and the root's is zero.** `capture` returns `None` for a
miss — compare with `is None`, never with truthiness, because `0` is falsy in Python
and the root's handle is 0.

**A component's globals are shared by every rule it hosts.** The host instantiates once
per (worker, component). Anything outliving a `check` call must be derivable from that
call's inputs — a memo keyed on the file path qualifies; a counter of files seen does
not. Only `configure`'s options are per rule index.

## The determinism hazard you cannot see

CPython randomizes string hashes from a seed drawn during pre-init and frozen into the
heap image. **The seed is per artifact**: within one build, `hash('lanekeep')` and set
iteration order are stable; across builds of identical source, both change. A rule that
iterates a `set` and reports "the first" of it, or stops after N, reports a different
violation from a rebuild of its own source. Sorting violations by
`(ruleId, file, line, column)` does not rescue it.

There is no runtime reset — the seed is baked in, and `PYTHONHASHSEED` in the host
environment cannot help because pre-init runs CPython *inside* the guest. The mitigation
is authoring discipline: **sort (`sorted()`) when order matters, and never pick "the
first" of a set or stop after N.** `crates/lanekeep-wasm/tests/python_determinism.rs`
demonstrates both halves against two builds of one source.
