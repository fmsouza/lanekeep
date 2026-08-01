# Writing a cross-file rule

Most rules decide from one file. Some cannot: whether an export is unused, whether imports
form a cycle, whether two packages agree on a version — none of those are properties of the
file you are looking at.

Those rules run in two phases. `check` sees one file at a time and emits **facts**. `reduce`
sees every fact and reports.

```ts
import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-unused-exports',
  query: `
    (export_statement declaration: (function_declaration name: (identifier) @name)) @stmt
    (import_statement (import_clause (named_imports (import_specifier name: (identifier) @imported))))
  `,
  card: {
    message: 'unused export',
    remediation: 'delete it, or import it somewhere',
    examples: { bad: 'export function unused() {}', good: 'function used() {}' },
  },

  check(ctx, m) {
    if (m.imported) {
      ctx.emitFact({ kind: 'import', symbol: ctx.text(m.imported) })
      return
    }
    ctx.emitFact({
      kind: 'export',
      symbol: ctx.text(m.name),
      line: ctx.line(m.stmt),
      column: ctx.column(m.stmt),
    })
  },

  reduce(ctx) {
    const imported = new Set(ctx.facts('import').map((f) => f.symbol))
    for (const e of ctx.facts('export')) {
      if (!imported.has(e.symbol)) {
        ctx.report(
          { file: e.file, line: e.line, column: e.column },
          `'${e.symbol}' is exported but never imported`,
        )
      }
    }
  },
})
```

## The two contexts are different on purpose

`check` and `reduce` receive different `ctx` objects, and neither is a subset of the other.

| | `check` | `reduce` |
| --- | --- | --- |
| `ctx.emitFact` | yes | **no** |
| `ctx.facts`, `ctx.files` | **no** | yes |
| the tree — `ctx.text`, `ctx.kind`, `ctx.parent`, … | yes | **no** |
| `ctx.report` | at a node | at `{ file, line, column }` |

Two rules explain the whole table:

**`reduce` never sees a parse tree.** That is invariant 1. If it did, running any cross-file
rule would make the whole corpus resident at once and force every file to be parsed on every
run — which is the cost the cache exists to avoid. Facts are small, they are JSON, and they
sit in the cache entry beside the violations for the file that produced them. A warm run that
reparses nothing can still reduce over the full corpus.

**`check` never sees the corpus.** If it could, a file's result would depend on files other
than itself, and caching that result against its own content would be unsound.

## Capture positions during `check`

There are no nodes in `reduce`, so a position has to be recorded while the tree still exists.
`ctx.line(node)` and `ctx.column(node)` give plain numbers; put them on the fact.

A fact must survive `JSON.stringify` — that is what makes it cacheable, and it is checked at
emit time rather than trusted. Node handles are arena indices for one file and mean nothing
later; do not store one expecting to use it.

## What the host controls

- **`file` is attached by the host**, and attached last, so a rule cannot make a violation
  appear to come from somewhere it did not.
- **`kind` is required** and must be a non-empty string. It is what `ctx.facts(kind)` selects
  on, so a fact without one could never be read back — accepting it would leave the rule
  looking correct right up until `reduce` found nothing.
- **Order is fixed**: facts arrive sorted by `(file, emission order)`. Files are checked in
  parallel, so without this a rule that stops at the first match, or builds a "first seen
  wins" map, would give different answers on different runs.
- **A rule sees only its own facts.** Reading another rule's would turn a private payload
  shape into a contract between rules, and would make results depend on declaration order.

## Budgets

`reduce` runs under the same per-invocation timeout as `check` — one second by default —
except it processes the whole corpus rather than one file. Raise it with the rule's `timeout`
field when the work genuinely warrants it:

```ts
export default defineRule({
  timeout: 5_000,
  // ...
})
```

Breaching it cancels the run and exits `2`. It does not skip the rule: a timeout is
timing-dependent, so a rule that trips on a loaded machine and not on an idle one would make
output vary between runs on identical input.

## When you do not need this

If a rule can decide from one file, decide from one file. The two-phase form costs a second
pass, a serialization round trip, and a fact set proportional to the corpus. `check` alone is
cheaper, simpler, and caches per file without any of this machinery.
