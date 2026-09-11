# Writing an obligation rule

Most rules answer a question about one place in the tree: does this node match, does this
call look wrong. An obligation rule answers a question about *every path out of a scope*:
once a value is acquired, does a release happen no matter which way control leaves? That is
a typestate question, and answering it needs the function's control-flow graph, not just a
query match — see [`architecture.md`](architecture.md) §6.11 for how it fits beside the rest
of the host API.

```ts
import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/secrets-zeroed-on-all-paths',
  requires: ['dataflow'],

  obligation: {
    acquire: [
      `(call_expression
         function: (member_expression property: (property_identifier) @m)
         (#any-of? @m "getEntropy" "deriveSeed")) @acquire`,
    ],
    release: [
      `(call_expression
         function: (member_expression property: (property_identifier) @p)
         (#eq? @p "fill")
         arguments: (arguments (number) @z) (#eq? @z "0")) @release`,
      `(call_expression function: (identifier) @f (#eq? @f "zeroBytes")) @release`,
    ],
    scope: 'function',
  },

  card: {
    message: 'secret buffer not zeroed on all paths',
    remediation: 'call .fill(0) or zeroBytes on every path, e.g. in finally',
    examples: {
      bad: 'const b = e.getEntropy();',
      good: 'const b = e.getEntropy(); try { /* use b */ } finally { b.fill(0); }',
    },
  },

  checkObligation(ctx, unmet) {
    ctx.report(unmet.exit, unmet.partial ? 'zeroed on some paths, not all' : 'never zeroed')
  },
})
```

No `query` and no `check` here — `obligation`/`checkObligation` is a complete rule shape on
its own, driven entirely by the acquire and release queries. A rule may still declare both:
an ordinary `check` for one property and `obligation`/`checkObligation` for another, over the
same file.

## The shape

```ts
type ObligationSpec = {
  acquire: string[]
  release: string[]
  scope: 'function' | 'block' | 'module'
}
```

`acquire` and `release` are each a list of tree-sitter queries, compiled the same way the
rule's main `query` is. Each is a list rather than a single string so that more than one call
shape can start or end the obligation — the example above releases through either `.fill(0)`
or a `zeroBytes(...)` helper, and either one discharges it. A capture literally named
`@acquire` or `@release` is what the analyzer reads out of a match; name it that in every
query, however the rest of the pattern is shaped. A query in either role may also bind an
optional `@key` capture alongside it, to correlate a specific acquire with a specific release
rather than treating every release as interchangeable — see "`@key` correlation and
`scope: 'module'`" below.

`scope` decides which paths have to carry a release, or — for `'module'` — whether a matching
one exists at all:

- **`'function'`** — every path out of the function the acquire sits in, `return` and
  `throw` included.
- **`'block'`** — every path out of the lexical block the acquire sits in, which is stricter:
  a release textually after the block does not count even if it is the very next statement.
- **`'module'`** — no paths, and no control-flow graph at all: discharge is whether a release
  keyed to the same `@key` exists anywhere in the file. This scope requires `@key` — see
  below.

## `requires: ['dataflow']` is mandatory

Declaring `obligation` without also declaring `requires: ['dataflow']` is refused at load —
the capability has to be visible in the rule's own header, not merely implied by the field
being present. The load-time refusals, all naming the rule:

| Mistake | Result |
| --- | --- |
| `obligation` with no `checkObligation` | refused — it could never fire |
| `checkObligation` with no `obligation` | refused — nothing drives it |
| `obligation` with no `requires: ['dataflow']` | refused — the capability must be declared |
| `acquire` or `release` empty or absent | refused — nothing to acquire, or nothing to discharge it, means `checkObligation` could never say anything true |
| an `acquire` query with no `@acquire`, or a `release` query with no `@release` | refused — it would compile and match nothing forever |
| `scope` missing | refused — the type declares it required, and the loader says the same rather than defaulting to `'function'` |
| `scope` other than `'function'`/`'block'`/`'module'` | refused |
| `@key` bound on some acquire/release queries but not every one | refused — correlation has to be all-or-nothing across the whole obligation, never partial |
| `scope: 'module'` without `@key` bound on every acquire and release query | refused — a file-wide scope with no correlation would let any release in the file discharge any acquire |
| an acquire or release query that fails to compile | refused, naming the rule, at the same point a broken main `query` is |
| neither `check` nor `obligation` | refused — a rule needs a handler |

A rule targeting a language with no obligation analyzer is not on this list — it loads
cleanly and is silent at run time instead. See Limitations below.

## What `checkObligation` receives

```ts
type UnmetObligation = {
  readonly acquire: Node
  readonly exit: Node
  readonly partial: boolean
  readonly key?: Node
}
```

| Field | Meaning |
| --- | --- |
| `acquire` | The node the acquire query matched. |
| `exit` | The source-earliest `return`, `throw`, or implicit function end reachable from the acquire without passing a release. This is what `ctx.report` is usually called on — the escape the analysis found, not the acquire itself. Under `scope: 'module'` there is no path to walk, so this is always the acquire node itself. |
| `partial` | Whether *some* path did discharge the obligation. A resource zeroed on the happy path but missed on one early `return` is a different finding from one never zeroed at all, and `partial` is how a rule tells the two apart in its message. Always `false` under `scope: 'module'` — there are no paths to be partial over. |
| `key` | The acquire's `@key` capture, present when the rule's acquire and release queries bind one — absent for an un-keyed obligation. `ctx.text(unmet.key)` is how a rule names the value in its own message; see the worked example below. |

`checkObligation` is called once per acquire the analysis cannot prove discharged — nothing
is called for an acquire that is released on every path.

## Worked behavior, `scope: 'function'`

Run against the rule above, over `scope: 'function'`:

| Code | Result |
| --- | --- |
| `const b = e.getEntropy(); if (c) { return; } b.fill(0);` | reports, `partial: true` — the early `return` skips the fill |
| `const b = e.getEntropy(); try { use(b); } finally { b.fill(0); }` | silent — `finally` runs on every path out of the `try` |
| `const b = e.getEntropy(); if (c) { throw x; } b.fill(0);` | reports, `partial: true` — the `throw` path never reaches the fill |
| `const b = e.getEntropy();` | reports, `partial: false` — no release anywhere |
| `const b = e.getEntropy(); if (c) { b.fill(0); } else { b.fill(0); }` | silent — both branches discharge it |
| `const b = e.getEntropy(); for (const x of xs) { b.fill(0); }` | reports, `partial: true` — the loop body may run zero times |
| `const b = e.getEntropy(); zeroBytes(b);` | silent — the second `release` query matches |

The first and the fourth row both report, and `partial` is the only thing that tells them
apart: `true` for "some path got it right", `false` for "no path ever does." A message that
only says "not released" reads identically for both; the worked rule above puts `partial` in
the text for exactly this reason.

## `scope: 'block'`

```ts
obligation: {
  acquire: ['(call_expression) @acquire'],
  release: ['(call_expression) @release'],
  scope: 'block',
}
```

| Code | Result |
| --- | --- |
| `{ const b = acquire(); release(b); } after();` | silent — the release is lexically inside the acquire's own block |
| `{ const b = acquire(); } release();` | reports — the release sits outside the block, so it cannot be what discharges the obligation inside it, even though nothing about the control flow itself forces the two into separate graph blocks |

## `@key` correlation and `scope: 'module'`

Everything above is silent about *which* value a release let go of: a release on all paths
discharges every acquire it is on-all-paths-from, whichever acquire that was. `@key` is how a
rule says the two have to be the same value, and `scope: 'module'` is the scope that needs it
— the register/forget pattern below has no control-flow graph to share in the first place,
because the acquire and the release sit in two different, unrelated functions.

```ts
export default defineRule({
  id: 'local/registered-is-forgotten',
  requires: ['dataflow'],
  obligation: {
    acquire: [
      `(call_expression function: (identifier) @f (#eq? @f "reg")
         arguments: (arguments (identifier) @key)) @acquire`,
    ],
    release: [
      `(call_expression function: (identifier) @f (#eq? @f "forget")
         arguments: (arguments (identifier) @key)) @release`,
    ],
    scope: 'module',
  },
  card: {
    message: 'not forgotten',
    remediation: 'call forget(id)',
    examples: { bad: 'reg(a)', good: 'reg(a); forget(a)' },
  },
  checkObligation(ctx, unmet) {
    ctx.report(unmet.acquire, `registration for ${ctx.text(unmet.key)} is never forgotten`)
  },
})
```

Both queries bind `@key` on the same argument position, alongside the `@acquire`/`@release`
capture every obligation query needs. Correlation is exact source-text equality on whatever
`@key` captured — `reg(id)` and `forget(id)` correlate because the text between the
parentheses matches, not because the analysis traced `id` to a declaration or a binding.

| Code | Result |
| --- | --- |
| `const on = (id) => { reg(id); };` | reports — `registration for id is never forgotten`; no `forget` anywhere in the file |
| `const on = (id) => { reg(id); }; const off = (id) => { forget(id); };` | silent — the sibling arrow's `forget(id)` matches the key, even though the two functions share no control-flow graph at all |
| `const on = (id) => { reg(id); }; const off = (other) => { forget(other); };` | reports — a `forget` exists, but its key text does not match `id` |

Order does not matter: a `forget(id)` written before its `reg(id)` still discharges it, since
`scope: 'module'` checks existence, not reachability.

`@key` is not exclusive to `scope: 'module'` — a `'function'`- or `'block'`-scoped obligation
may bind it too, and the release set the CFG walk considers is filtered down to matching-key
releases before that walk runs. That is what makes `const a = acq(); const b = acq(); rel(a)`
report only `b`, once both `acq`'s and `rel`'s queries key on the acquired value: without
`@key` a release discharges every acquire it is on-all-paths-from regardless of which value
came back, and `a` would silently cover for `b`. `scope: 'module'` is the one place `@key`
stops being optional: without it a file-wide scope would let any release in the file discharge
any acquire, which is strictly worse than a scope that at least confines itself to one
function — see the load-time refusals above.

## Limitations

Each of these is a stated v1 scope decision, not an oversight — see
[`architecture.md`](architecture.md) §6.11 for the mechanism behind each one.

- **Value identity is opt-in, through `@key`.** Bind an optional `@key` capture on every
  acquire and release query — see "`@key` correlation and `scope: 'module'`" above — and
  discharge requires matching key text, not merely a release somewhere on all paths.
  `scope: 'module'` requires it outright: a file-wide scope with no correlation would let any
  release discharge any acquire, which is strictly worse than a narrower scope. Without
  `@key`, nothing here has changed: a release on all paths still discharges every acquire it
  is on-all-paths-from, regardless of which value it released — exact with one acquire per
  function and imprecise with several, do not rely on it to tell two acquired values in the
  same function apart. And keyed or not, there is still no notion that `return`/`throw`
  themselves release: a release is only ever a query match, never inferred from a value
  handed back to a caller that might release it instead.
- **Nothing crosses a function boundary under `'function'`/`'block'` scope.** The unit of
  analysis there is the function the acquire is in; a callback or a call passed the acquired
  value is invisible to the CFG walk. `scope: 'module'` is the deliberate exception — it exists
  because this is exactly what makes a register/forget pair split across two sibling callbacks
  inexpressible otherwise, and it buys that by giving up the control-flow graph entirely for a
  file-wide existence check keyed by `@key`.
- **Silent, not refused, on a language with no analyzer.** In v1 the analyzer exists only for
  TypeScript, TSX and JavaScript. Declaring `obligation` for any other language is not a load-time
  mistake — the rule loads cleanly, and `checkObligation` is simply never invoked for that
  language's files, the same quiet-absence posture `ctx.types` takes when it has nothing to
  say. A `check` the same rule also declares is unaffected and still runs.
- **`--fix` is not offered.** `checkObligation` gets the same `ctx.report(node, { fix })`
  `check` does, so nothing stops a handler from attaching a `Fix` to `unmet.exit` or
  `unmet.acquire` — it is just a poor fit: a fix replaces one node's text, and the remedy is
  almost always a `finally` that does not exist yet, not a replacement of the node the
  violation is reported on. Put the whole remedy in `card.remediation` instead.
- **Not skipped by `--since`, `--staged` or `--file`.** Unlike a cross-file `reduce` rule, an obligation
  rule is per-file and needs no whole-corpus view, so it runs — and can find something — over
  however small a set of changed files you give it. It is safe, and useful, in a pre-commit
  hook.

## When you do not need this

If the property can be checked with an ordinary query — two calls that both have to appear
somewhere in the function, with no question of *which paths* connect them — write that
instead. `obligation` costs a control-flow graph build per matched function, and it only
earns that cost when the answer genuinely depends on every path out of a scope rather than on
whether two calls both happen to be present somewhere in it.
