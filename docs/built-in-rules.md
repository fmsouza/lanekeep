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

## Adding one

A built-in gets no privileged path into the engine, which is the point: a built-in that
needed something a project rule cannot have would be evidence the host API is wrong.

1. Write `crates/lanekeep-rules/rules/<name>.ts`, importing only from `lanekeep`.
2. Add it to `BUILT_INS` in [`crates/lanekeep-rules/src/lib.rs`](../crates/lanekeep-rules/src/lib.rs).
3. Test it in `crates/lanekeep-rules/tests/<name>.rs` with `RuleTester`, which runs the real
   engine over a throwaway project — real config loading, real gates, real sandbox.

Cover the forms that do not look like the obvious one. Every gap found while writing these
two rules was a form that read differently in source but meant the same thing.
