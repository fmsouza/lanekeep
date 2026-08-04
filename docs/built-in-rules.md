# Built-in rules

The rules lanekeep ships with. They are authored in TypeScript against the same host API
project rules use, and embedded in the binary — nothing is installed, and there is no
`node_modules` to resolve.

Import one by specifier and put it in your `rules` array:

```ts
import { defineConfig } from 'lanekeep'
import noDefaultExport from 'lanekeep/no-default-export'

export default defineConfig({
  include: ['src/**'],
  rules: [noDefaultExport],
})
```

Built-ins resolve before the filesystem is consulted, so a file at
`lanekeep/no-default-export.ts` in your project does not shadow one. A rule whose behavior
depended on whether a same-named file happened to exist would be unreasonable to debug.

Built-in ids are namespaced `lanekeep/`. Project rules use `local/`, or a namespace the
project declares in its config — `namespaces: ['acme']` allows `acme/no-numeric-sizes`.
`lanekeep/` stays reserved, so a rule's origin is readable from its id alone. That is one of
the one-way doors in [`architecture.md`](architecture.md) §14 — it is what lets a suppression
comment, a config override, or a JSON consumer name a rule unambiguously forever.

**Every rule targets one or more languages**, and a rule does not run on a file whose language
it does not name. The rules below are grouped by the language they are about; each says which.
A rule that names no language defaults to `['typescript', 'tsx']`.

---

# TypeScript and JavaScript

## `lanekeep/no-default-export`

Named exports only.

A default export has no canonical name. Every importer picks its own, so the same symbol
appears as three different identifiers across a codebase and neither grep nor a rename
refactor finds all of them. The cost is invisible in any single file, which is why it needs
a rule rather than a review comment.

Takes no options.

```ts
import noDefaultExport from 'lanekeep/no-default-export'

export default defineConfig({ rules: [noDefaultExport] })
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

It is a **factory** — a function you call with options, which returns a rule. The
restriction is the entire content of the rule, so there is nothing useful to import
unconfigured.

```ts
import noRestrictedImports from 'lanekeep/no-restricted-imports'

export default defineConfig({
  rules: [
    noRestrictedImports({
      restrictions: [
        {
          module: 'stripe',
          from: ['!packages/payments/**'],
          reason: 'route payments through the payments package',
        },
        { module: 'lodash/*', reason: 'use the standard library' },
      ],
    }),
  ],
})
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

```ts
import noUnusedExports from 'lanekeep/no-unused-exports'

export default defineConfig({
  rules: [noUnusedExports({ entryPoints: ['src/index.ts', 'src/cli.ts'] })],
})
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

```ts
import noCircularImports from 'lanekeep/no-circular-imports'

export default defineConfig({
  rules: [noCircularImports()],
})
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
surfaces far from its cause.

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
build it, and cancelling that request cancels work belonging to every other.

Both `context.Context` and `*context.Context` are reported; they differ by a `pointer_type` in
the tree, so each needs its own query pattern.

A qualifier that is not an import does not fire — a package-level name that happens to read
`context` is not the standard library.

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
order neither states, and the failure moves when an unrelated import is added.

It is also how a package acquires hidden startup cost: an import that looks free opens a
connection or reads a file.

Every `init` in a file is reported; Go permits several, which is what makes the ordering hard
to reason about in the first place. A *method* named `init`, or a variable holding a function
literal, is not reported — neither is called implicitly, so neither has the property the rule
objects to.

---

# Rust

Both Rust rules are about *legibility of dependencies and failure*: where a name came from, and
what happens when something goes wrong.

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
default. `allow` takes patterns to widen that, matched against the wildcard's *full* text —
`super::*`, not `super`, for `use super::*;`.

```json
{ "rule": "lanekeep/no-glob-import", "options": { "allow": ["*prelude*", "super::*"] } }
```

**The object form is required.** This rule is a factory — that is what makes `allow`
reachable at all — so a bare `"lanekeep/no-glob-import"` fails config load with
`missing 'id'` rather than running with the default `allow`.

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

```json
{ "rule": "lanekeep/no-unwrap", "options": { "allow": ["src/main.rs"] } }
```

**The object form is required.** This rule is a factory — that is what makes `allow`
reachable at all — so a bare `"lanekeep/no-unwrap"` fails config load with `missing 'id'`
rather than running with no exemptions.

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

Cover the forms that do not look like the obvious one. Every gap found while writing these
two rules was a form that read differently in source but meant the same thing.
