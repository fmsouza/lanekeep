# Built-in rules

The rules lanekeep ships with. They are authored against the same host API project rules use,
and embedded in the binary — nothing is installed, and there is no `node_modules` to resolve.

Name one by specifier and put it in your `rules` array. In a `lanekeep.json`, which is what
`lanekeep init` writes:

```json
{
  "include": ["src/**"],
  "rules": ["lanekeep/no-default-export"]
}
```

Or in a `lanekeep.config.ts`, for a rule that is a TypeScript module — which today means the two
Python rules and nothing else:

```ts
import { defineConfig } from 'lanekeep'
import noBroadExcept from 'lanekeep/no-broad-except'

export default defineConfig({
  include: ['src/**'],
  rules: [noBroadExcept],
})
```

Built-ins resolve before the filesystem is consulted, so a file at
`lanekeep/no-broad-except.ts` in your project does not shadow one. A rule whose behavior
depended on whether a same-named file happened to exist would be unreasonable to debug.

**Eight of the ten are compiled rules rather than TypeScript modules, and a `lanekeep.config.ts`
cannot import one.** The two Rust rules — `lanekeep/no-glob-import` and `lanekeep/no-unwrap` —
the two Go ones — `lanekeep/no-context-in-struct` and `lanekeep/no-package-init` — and the four
TypeScript ones — `lanekeep/no-default-export`, `lanekeep/no-restricted-imports`,
`lanekeep/no-circular-imports` and `lanekeep/no-unused-exports` — are WebAssembly components.
They have no JavaScript left to import at run time, and they describe themselves rather than
being read out of a `defineRule` call.

**Name them from a `lanekeep.json` and everything below works the same, options included.**
Importing one from a TypeScript config fails at load with a message that says so and names the
remedy — which is the whole of the difference a user sees. Every example in this document uses
whichever format the rule it documents accepts.

The four TypeScript rules are compiled from *exactly* the sources they were written in; nothing
about them changed but the engine that runs them. The Rust and Go ones are written in the
language they check and have no TypeScript at all. Which form a rule takes is not part of its
interface: the specifier, the id, the options and the output are the same either way, and a rule
that changes form does not change your config — the two Go rules were TypeScript modules until
`fec6cfc` and every case in their test suites passed unchanged across the move.

Built-in ids are namespaced `lanekeep/`. Project rules use `local/`, or a namespace the
project declares in its config — `namespaces: ['acme']` allows `acme/no-numeric-sizes`.
`lanekeep/` stays reserved, so a rule's origin is readable from its id alone. That is one of
the one-way doors in [`architecture.md`](architecture.md) §14 — it is what lets a suppression
comment, a config override, or a JSON consumer name a rule unambiguously forever.

**Every rule targets one or more languages**, and a rule does not run on a file whose language
it does not name. The rules below are grouped by the language they are about; each says which.
A rule that names no language defaults to `['typescript', 'tsx']`.

---

# TypeScript

**These four run on `.ts` and `.tsx` and not on JavaScript.** None of them declares a `language`,
so all four take the default above — `['typescript', 'tsx']` — and `javascript` is a separate
language covering `.js`, `.mjs`, `.cjs` and `.jsx`. This section was headed "TypeScript and
JavaScript" for a while and the rules underneath it never fired on a `.js` file, which is the
quiet kind of wrong: a rule that does not run looks exactly like a codebase with nothing to
report. Extending them is a rule change rather than a documentation one.

## `lanekeep/no-default-export`

Named exports only.

A default export has no canonical name. Every importer picks its own, so the same symbol
appears as three different identifiers across a codebase and neither grep nor a rename
refactor finds all of them. The cost is invisible in any single file, which is why it needs
a rule rather than a review comment.

Takes no options.

```json
{ "rules": ["lanekeep/no-default-export"] }
```

It reports all four forms, including the two that do not contain the words `export default`:

```ts
export default function parse() {}     // reported
export default class Parser {}          // reported
export { parse as default }             // reported — aliased to default
export { default } from './parse'       // reported — a default re-exported as default
```

And allows the form that removes a default rather than creating one:

```ts
export { default as parse } from './parse'   // fine: takes a default in, publishes a name
```

The violation is reported at the token that has to change — the statement for the keyword
form, the `default` token itself for the export-clause forms.

---

## `lanekeep/no-restricted-imports`

Forbid importing given modules, optionally only from given paths.

This is the workhorse architectural rule: "the UI layer must not reach into the database
client", "nothing outside `payments/` may import the Stripe SDK". Those are exactly the
conventions a language model cannot infer from the code it is shown.

The restriction is the entire content of the rule, so naming it without options does nothing
useful — give it `restrictions`:

```json
{
  "rules": [
    {
      "rule": "lanekeep/no-restricted-imports",
      "options": {
        "restrictions": [
          {
            "module": "stripe",
            "from": ["!packages/payments/**"],
            "reason": "route payments through the payments package"
          },
          { "module": "lodash/*", "reason": "use the standard library" }
        ]
      }
    }
  ]
}
```

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `restrictions` | `Restriction[]` | What is forbidden. Defaults to `[]`. |

Each restriction:

| Field | Type | Meaning |
| --- | --- | --- |
| `module` | `string` | The specifier to forbid. `*` matches anything, including `/`. |
| `from` | `string[]` | Where the restriction applies. Omitted means everywhere. |
| `reason` | `string` | What to do instead. Carried into the violation message. |

`module` is matched against the specifier **as written**, not against a resolved path. That
is deliberate: a restriction is written against what an author types, and resolving first
would make `lodash/*` fail to match `lodash/merge`.

An entry in `from` beginning with `!` is a carve-out — the restriction applies everywhere
*except* there, and a carve-out beats an inclusion. That inversion is what makes "nothing
may import Stripe outside `packages/payments`" one restriction rather than an enumeration of
every other directory, which would rot the first time someone adds one.

`reason` is the field worth spending time on. An agent reading lanekeep's output needs to
know what to do instead, not merely that something is banned — the reason is the part of the
message that tells it.


---

## `lanekeep/no-unused-exports`

Exports nobody in the corpus imports.

Dead exported code is worse than dead private code: it is part of a module's surface, so a
reader has to assume something depends on it, and an agent will happily wire new code up to
it. Nothing in the exporting file reveals the problem — which is why this is a
[cross-file rule](cross-file-rules.md) rather than a query.

```json
{
  "rules": [
    {
      "rule": "lanekeep/no-unused-exports",
      "options": { "entryPoints": ["src/index.ts", "src/cli.ts"] }
    }
  ]
}
```

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `entryPoints` | `string[]` | Files whose exports are consumed from outside the corpus. Defaults to `[]`. |

**Set `entryPoints`.** A library's public API is imported by its users, not by itself, so
without this the rule reports every exported symbol of every package — true of the corpus,
and useless as advice.

### What counts as a use

Matching is by `(resolved module, symbol)`, not by symbol name. A rule that treated any
`parse` imported anywhere as covering every exported `parse` would go quiet on a codebase
with common names, which is every codebase.

- `import { x } from './a'` uses `a.ts`'s `x`.
- `import { x as y } from './a'` uses `x` — the name, not the alias. It is `x` that `a.ts`
  publishes.
- `import * as ns from './a'` and `export * from './a'` consume the whole module without
  naming anything, so everything in `a.ts` counts as used. The alternative would be
  reporting exports that are demonstrably reachable.
- `export { internal as published }` is reported under `published`, the name it publishes.
- A specifier that resolves to nothing in the corpus — a package, an excluded file — is
  ignored rather than guessed at.

Relative specifiers resolve without extensions (`./a` finds `a.ts`), through directories
(`./thing` finds `thing/index.ts`), and across `..`. There is no `node_modules` lookup and
no `tsconfig` path mapping: a specifier that does not start with `.` names something outside
the corpus, and a rule reasoning about the corpus has nothing to say about it.

---

## `lanekeep/no-circular-imports`

Import cycles.

A cycle is the architectural failure that hides best. Everything compiles, the bundler
copes, and then one module observes a half-initialized binding from another and produces an
error miles from its cause — usually only under a particular import order, which is why it
survives review and shows up in production.

```json
{ "rules": ["lanekeep/no-circular-imports"] }
```

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `maxDepth` | `number` | Longest cycle to look for. Defaults to `24`. |

A cycle spanning more files than `maxDepth` is not reported. Cycles get harder to act on the
longer they are, and an unbounded search on a pathological graph is the one way this rule
could become the slowest thing in a run.

### What it reports

One violation per cycle, not per member — `a → b → c → a` is one problem, and reporting it
three times would leave the reader to work out they are the same thing. The violation is
anchored at the import that closes the cycle: the single edge whose removal breaks it.

`export ... from` counts as an edge. It is an import with different syntax, and a cycle
through one fails at runtime the same way.

A module importing itself is not reported. It is a different mistake, and "extract what both
modules need into a third" is not advice that applies to it.

# Python

Both Python rules resolve identifiers rather than matching text, which is what keeps them
from firing on a project that has taken the name for something of its own.

## `lanekeep/no-broad-except`

Catch what the block can actually raise.

A bare `except:` catches `KeyboardInterrupt` and `SystemExit` too, so it swallows the user
pressing Ctrl-C. `except Exception:` is narrower and still catches every bug in the block — a
typo'd attribute, a `None` where an object was expected — and reports whatever the handler
decided the failure was. The error the code was written to handle and the error nobody
anticipated come out identical.

Takes no options.

```ts
import noBroadExcept from 'lanekeep/no-broad-except'

export default defineConfig({ rules: [noBroadExcept] })
```

```python
try:
    parse(raw)
except:                    # reported — also catches Ctrl-C
    return None

try:
    parse(raw)
except Exception:          # reported — catches every bug in the block
    return None

try:
    parse(raw)
except Exception as err:   # reported — binding it does not narrow it
    return None

try:
    parse(raw)
except ValueError:         # fine — names what this block raises
    return None
```

A project that defines or imports its own `Exception` is not catching the builtin, and is not
reported. That is `ctx.bindingKind` doing the work a text match could not.

## `lanekeep/no-mutable-default-argument`

Defaults are evaluated once, at definition.

`def f(items=[])` builds one list when the function is defined, and every call that omits
`items` shares it. The second call sees what the first appended. It reads as a per-call
default and is not one, which is what makes it a rule rather than a review comment: the code
looks right, and the bug surfaces later and somewhere else.

Takes no options.

```ts
import noMutableDefaultArgument from 'lanekeep/no-mutable-default-argument'

export default defineConfig({ rules: [noMutableDefaultArgument] })
```

```python
def add(item, items=[]):        # reported
def f(opts={}):                 # reported
def f(seen={1}):                # reported
def f(items=list()):            # reported — the constructor spelling
def add(item, items=None):      # fine
def f(a=1, b='x', c=(), d=False):  # fine — none of these are mutable
```

A project that defines its own `list`, `dict` or `set` is not calling the builtin, and is not
reported.

---

# Go

Both Go rules are about *implicit* structure — a dependency the code does not state, and an
ordering nothing writes down. Neither is a style preference: each produces a failure that
surfaces far from its cause, which is what makes it worth stating as a project convention
rather than arguing case by case in review.

**Both are components**, like the two Rust rules and the four TypeScript ones, so name them from
a `lanekeep.json`:

```json
{ "rules": ["lanekeep/no-context-in-struct", "lanekeep/no-package-init"] }
```

They are written in the language they check, in [`go-rules/`](../go-rules), and compiled to
WebAssembly with TinyGo — see [`authoring-go-rules.md`](authoring-go-rules.md). Neither takes
options. Both resolve identifiers rather than matching text, which is what keeps them from
firing on a project that has taken a name for something of its own.

## `lanekeep/no-context-in-struct`

Pass a context, do not store one.

```go
// bad
type Client struct {
	ctx context.Context
}

// good
type Client struct{}

func (c *Client) Do(ctx context.Context) error { return nil }
```

The context package says it directly: do not store Contexts inside a struct type. A stored
context outlives the call it was scoped to, so cancellation and deadlines stop meaning what
the caller intended — a long-lived client holds the context of whichever request happened to
construct it, and cancelling that request cancels work belonging to every other.

It is architectural rather than stylistic because of how the damage arrives: as unrelated
requests failing together, which is nearly impossible to attribute back to the field that
caused it.

Both `context.Context` and `*context.Context` are reported; they differ by a `pointer_type` in
the tree, so each needs its own query pattern.

`ctx.bindingKind` is what keeps this from being a text match: it says whether the qualifier is
an import at all, so a package-level name that happens to read `context` does not fire.

### What it cannot tell apart

`ctx.bindingKind` says whether a name is an import; it does not say *which module*. A package
aliased to `context` and exposing a `Context` is therefore reported like the real one:

```go
import context "example.com/app/context"   // reported, though it is a different package
```

Distinguishing them needs the host API to expose an import's module, which bumps the host API
version and so the cache key — a larger change than this rule justifies on its own. The case
is narrow, and where it occurs it is usually the same mistake under a different import path.
A test pins the behavior, so tightening the rule later fails loudly rather than silently.

## `lanekeep/no-package-init`

Wire things up where the wiring is visible.

```go
// bad
func init() {
	registry["pg"] = newPostgres()
}

// good
func Register(r map[string]Driver) {
	r["pg"] = newPostgres()
}
```

An `init` function runs when the package is imported, before `main`, in an order the language
decides. Nothing calls it, so nothing in the code says when it happens — a reader tracing
startup finds no edge leading to it. Two packages registering into a shared map depend on an
order neither states, and the failure — a missing registration, a nil global — surfaces far from
the cause and moves when an unrelated import is added.

It is also how a package acquires hidden startup cost: an import that looks free opens a
connection or reads a file.

Wiring done explicitly from `main` — or a `New...` returning an error — is traceable, testable,
and ordered by the code rather than by the linker.

Every `init` in a file is reported; Go permits several, which is what makes the ordering hard
to reason about in the first place. A *method* named `init`, or a variable holding a function
literal, is not reported — neither is called implicitly, so neither has the property the rule
objects to.

---

# Rust

Both Rust rules are about *legibility of dependencies and failure*: where a name came from, and
what happens when something goes wrong.

**Both are components**, like the four TypeScript rules above, so name them from a
`lanekeep.json`:

```json
{ "rules": ["lanekeep/no-unwrap", { "rule": "lanekeep/no-glob-import", "options": { "allow": ["*prelude*"] } }] }
```

They are written in the language they check, in [`rust-rules/`](../rust-rules), and compiled to
WebAssembly — see [`authoring-rust-rules.md`](authoring-rust-rules.md). Nothing else about them
differs: same ids, same options, same output.

## `lanekeep/no-glob-import`

```rust
// bad
use crate::models::*;

// good
use crate::models::{User, Session};
```

A glob makes it impossible to answer, by reading the file, where a name came from. Every
unqualified identifier becomes a candidate for every glob in scope, and the answer moves when an
upstream crate adds a public item — a name that resolved to yours last week resolves to theirs
today, with no change on your side.

It is also the case that defeats tooling. lanekeep's own resolver reports nothing for a glob
import, because the names it brings in cannot be known without reading the other crate, so a
rule asking "is this the imported `Result`?" quietly stops being answerable in any file with
one.

Preludes are the shape a glob is the intended spelling of, and `*prelude*` is allowed by
default. `allow` takes patterns to widen that.

## `lanekeep/no-unwrap`

```rust
// bad
let config = load().unwrap();

// good
let config = load()?;
```

A library that panics on a malformed input has failed at its job: the caller wanted an error it
could handle and got a process abort. In a binary it is a crash whose stack trace points at the
unwrap rather than at what was actually wrong.

**Test code is exempt.** `#[test]` functions, `#[cfg(test)]` modules and files under `tests/`
are not reported — panicking *is* the failure mechanism there, and reporting it would mean
either a rule nobody turns on or a suppression on every assertion. `allow` takes path patterns
for anything else, `src/main.rs` being the usual one.

### What it cannot tell apart

A method genuinely named `expect` on your own type — a mock builder, say — is reported like
`Result::expect`. Telling them apart needs type information, which lanekeep deliberately does
not have (§1 non-goals). A test pins the behavior so it is a known limit rather than a surprise.

---

## Composing them

The two cross-file rules share their module resolution, which is exported as
`lanekeep/paths` and available to project rules too:

```ts
import { resolveImport, dirname, join } from 'lanekeep/paths'
```

Two rules resolving `./a` differently would not look like a bug — each would be individually
plausible — so the resolution has one definition.

---

## Adding one

A built-in gets no privileged path into the engine, which is the point: a built-in that
needed something a project rule cannot have would be evidence the host API is wrong.

1. Write `crates/lanekeep-rules/rules/<name>.ts`, importing only from `lanekeep`.
2. Add it to `BUILT_IN_RULES` in [`crates/lanekeep-rules/src/lib.rs`](../crates/lanekeep-rules/src/lib.rs).
3. Test it in `crates/lanekeep-rules/tests/<name>.rs` with `RuleTester`, which runs the real
   engine over a throwaway project — real config loading, real gates, real sandbox. A
   cross-file rule needs more than one file, so those live in
   `crates/lanekeep-cli/tests/<name>.rs` and drive the built binary over a real corpus.

For a rule in Rust, steps 1 and 2 become `rust-rules/<name>/` and `BUILT_IN_COMPONENTS`;
[`authoring-rust-rules.md`](authoring-rust-rules.md) has the whole of it. Step 3 is the same
file with the same `RuleTester`, through `RuleTester::for_component`.

For a rule in Go, step 1 becomes `go-rules/rules/<name>/` plus a row in `go-rules/main.go`'s
`ruleset`, and step 2 becomes a `COMPONENT_RULES` row naming `go-builtins` and the index that
row sits at — every Go rule shares one component, so `BUILT_IN_COMPONENTS` already has its
entry. [`authoring-go-rules.md`](authoring-go-rules.md) has the whole of it, including the six
things about TinyGo that fail silently. Step 3 uses `RuleTester::for_built_in` rather than
`for_component`, for the reason the note below gives about a shared artifact.

To ship a TypeScript rule as a component instead of a module, step 1 is unchanged — it is the
same file, importing only from `lanekeep` — and steps 2 and 3 gain a build. List it in
`crates/lanekeep-rules/typescript/entry.ts`, whose order *is* the index the component is
dispatched on, record it in `COMPONENT_RULES` rather than `BUILT_IN_RULES`, and run
`just typescript-builtins` to rebuild the shared artifact and re-record its source digests. The
rule's own tests do not change, which is the point and was the acceptance test for the four that
moved: same source, same expectations, different engine.

**Add a test that runs the component, because "the tests do not change" does not mean they cover
it.** A test built on `lanekeep_rules::source(name)` runs the TypeScript in QuickJS whatever form
the rule ships in, so for two of the four the unchanged file went on testing the source and said
nothing about the artifact. `RuleTester::for_built_in` names the rule by its specifier —
`"lanekeep/no-default-export"` — which resolves through the embedded table to one rule of the
shared component; `RuleTester::for_component` cannot do this, because it writes the artifact to a
path and a path reference contributes every rule in it.
`crates/lanekeep-rules/tests/typescript_builtins_as_components.rs` is the worked example, and its
header explains why it is deliberately a subset: every case there compiles a 12.4 MiB artifact.

Cover the forms that do not look like the obvious one. Every gap found while writing these
two rules was a form that read differently in source but meant the same thing.
