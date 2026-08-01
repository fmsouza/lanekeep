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

Built-in ids are namespaced `lanekeep/`, project rule ids are namespaced `local/`. That is
one of the one-way doors in [`architecture.md`](architecture.md) §14 — it is what lets a
suppression comment, a config override, or a JSON consumer name a rule unambiguously
forever.

---

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
2. Add it to `BUILT_INS` in [`crates/lanekeep-rules/src/lib.rs`](../crates/lanekeep-rules/src/lib.rs).
3. Test it in `crates/lanekeep-rules/tests/<name>.rs` with `RuleTester`, which runs the real
   engine over a throwaway project — real config loading, real gates, real sandbox. A
   cross-file rule needs more than one file, so those live in
   `crates/lanekeep-cli/tests/<name>.rs` and drive the built binary over a real corpus.

Cover the forms that do not look like the obvious one. Every gap found while writing these
two rules was a form that read differently in source but meant the same thing.
