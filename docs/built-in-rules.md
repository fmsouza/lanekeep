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

Or in a `lanekeep.config.ts`, for a rule that is a TypeScript module — every built-in except
the four compiled ones, whatever language each targets:

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

**Four of the built-ins are compiled rules rather than TypeScript modules, and a `lanekeep.config.ts`
cannot import one.** The two Rust rules — `lanekeep/no-glob-import` and `lanekeep/no-unwrap` —
and the two Go ones — `lanekeep/no-context-in-struct` and `lanekeep/no-package-init` — are
WebAssembly components. They have no JavaScript left to import at run time, and they describe
themselves rather than being read out of a `defineRule` call.

**Name them from a `lanekeep.json` and everything below works the same, options included.**
Importing one from a TypeScript config fails at load with a message that says so and names the
remedy — which is the whole of the difference a user sees. Every example in this document uses
whichever format the rule it documents accepts.

The TypeScript-module rules are evaluated in QuickJS from the sources they were written in,
whatever language each targets (four of them briefly shipped compiled to a StarlingMonkey
component before that form was reverted for cost). The Rust and Go
ones are written in the language they check and have no TypeScript at all. Which form a rule
takes is not part of its interface: the specifier, the id, the options and the output are the
same either way, and a rule that changes form does not change your config — the two Go rules
were TypeScript modules until v0.7.0, and every case in their test suites passed
unchanged across the move. Their `.ts` sources are recoverable with
`git log --diff-filter=D -- crates/lanekeep-rules/rules/`, which is where a reader who wants
to compare the two implementations should start.

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

**The rules in this section run on `.ts` and `.tsx` and not on JavaScript.** Each either takes
the `['typescript', 'tsx']` default above or declares exactly that pair, and `javascript` is a
separate language covering `.js`, `.mjs`, `.cjs` and `.jsx`. This section was headed "TypeScript and
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

## `lanekeep/no-restricted-types`

Forbid a primitive type, or the wrong domain type, on a value whose name matches a naming
convention.

The first built-in where a *type*, not just syntax, decides the violation. The convention it
exists for: "every monetary value is a `Decimal` from `decimal.js`, and never a `number`, because
a `number` loses precision past 2^53" — that is not something a language model can infer from the
code it is shown, and it is the worked example through this section. `decimal-for-money` is not
hardcoded here: it ships as a *configuration* of this rule rather than as a rule of its own, on
the same reasoning `no-restricted-imports` already settled — the mechanism belongs in the tool,
the policy stays the project's.

A **factory**, for the same reason `no-restricted-imports` is one: the convention is the entire
content of the rule, so an empty `conventions` list reports nothing at all. It looks at function
parameters — required or optional — and variable declarations, and asks whether each one's name
matches a convention's `names`.

```json
{
  "rules": [
    {
      "rule": "lanekeep/no-restricted-types",
      "options": {
        "conventions": [
          {
            "names": ["*amount*", "*Amount*", "*balance*", "*price*"],
            "forbid": ["number", "string"],
            "require": { "module": "decimal.js", "name": "Decimal" },
            "reason": "number loses precision past 2^53"
          }
        ]
      }
    }
  ]
}
```

Or from a `lanekeep.config.ts`:

```ts
import noRestrictedTypes from 'lanekeep/no-restricted-types'

export default defineConfig({
  rules: [
    noRestrictedTypes({
      conventions: [
        {
          names: ['*amount*', '*Amount*', '*balance*', '*price*'],
          forbid: ['number', 'string'],
          require: { module: 'decimal.js', name: 'Decimal' },
          reason: 'number loses precision past 2^53',
        },
      ],
    }),
  ],
})
```

Its card: **message** `restricted type on a value the convention governs`, **remediation** "give
it the type the convention requires, or rename it if it is not what the name says", with the
example pair `function credit(amount: number)` (reported) and `function credit(amount: Decimal)`
(fine). The violation is anchored at the name, not at the type annotation.

**Two spellings fail to load.** A bare `"lanekeep/no-restricted-types"` in a JSON config, and an
uncalled `noRestrictedTypes` reference in a TypeScript config (`rules: [noRestrictedTypes]`
rather than `rules: [noRestrictedTypes({...})]`), both fail with ``missing `id` `` — a factory
function has no `id` of its own; only the rule it returns does.

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `conventions` | `Convention[]` | What is governed. Defaults to `[]`. |

Each convention:

| Field | Type | Meaning |
| --- | --- | --- |
| `names` | `string[]` | Glob patterns a parameter's or variable's name must match to be governed. |
| `forbid` | `string[]` | Primitive type names that are a violation on a governed value — `number`, `string`, `boolean`, `bigint`, `symbol`, `null` or `undefined`, the set the type oracle itself recognizes. |
| `require` | `{ module: string, name: string }` | Optional. The type that satisfies the convention. Only `module` is matched — a governed value's type must have a symbol imported from it. `name` is what the message says to use instead, and is not checked; see below for why. |
| `reason` | `string` | What to tell the reader. Carried into the violation message; falls back to naming `require`, then to a generic message, when it is absent. |

`require` is optional on its own terms: a convention may forbid a primitive without naming a
replacement — "never a raw `number` here" is a legitimate thing to say without saying what to use
instead. With no `require` to check against, a named type is accepted whatever it is; only a
listed primitive, or a union containing one, is still a violation.

### Values are selected by name, and the rule is only as good as that name

There is no inference here, and none is claimed: the rule cannot see that a value is money, only
that its name matches a convention's pattern. A monetary value called `total` slips past
`names: ['*amount*']` entirely, and a `maxRetryAmount` is caught as if it were money, because its
name happens to contain `Amount`. **A clean report from this rule is evidence the project's
naming held, not that its money is safe** — a reader who takes it as the latter is trusting a
report that means nothing.

### The match is case-sensitive, and here that is sharp

`names` is matched with `lanekeep/patterns`' `matches`, the same case-sensitive glob every other
built-in restriction list uses: `'*amount*'` does not match `totalAmount`, because the pattern's
lowercase `a` never matches the capital one a camelCase name puts there — and `totalAmount` is the
spelling a real codebase uses most. A convention meaning to catch every casing has to list both,
as the worked example above does (`'*amount*'` and `'*Amount*'` together). **A pattern that
silently matches nothing is the worst outcome this rule has**: nothing throws, nothing warns, and
the run reports clean exactly as if the convention were being enforced.

### It looks at parameters and variable declarations, and nothing else

The query matches a plain identifier bound by a parameter or a `const`/`let`/`var` declarator —
no wider. A class field, an interface or type-alias member, an object-literal property and a
destructured parameter are different grammar shapes, and none of them is a candidate at all:

```ts
class Order {
  amount: number // never a candidate — not a parameter, not a variable declarator
}
```

This is not the naming weakness above wearing a different hat, and it is not bounded by the type
oracle either: perfect naming and a perfect oracle still would not catch it, because the value is
never asked about in the first place. **If a project's money lives in a class field, this rule's
clean report means nothing** — a reader who has only been told that name matching is imperfect
will draw the wrong conclusion about why. Reaching these shapes would be a wider query, not a fix
to this one.

### What it checks, and what it stays silent on

The rule declares `requires: ['types']`, which is what puts `ctx.types` on its context at all —
see [`architecture.md`](architecture.md) §6.10. A rule that does not declare it pays nothing;
reading `ctx.types` without declaring it throws a `TypeError` at the first call instead of
quietly returning `undefined`, so an author who forgets the line finds out immediately rather than
shipping a rule that never reports.

Every governed name is asked what its type is:

| The oracle says | The rule does |
| --- | --- |
| a primitive listed in `forbid` | reports |
| a union with a member whose primitive is in `forbid` | reports |
| a union with no such member | silent |
| a named type, when the convention sets no `require` | silent — nothing to check it against |
| a named type whose symbol was imported from `require`'s module | silent |
| a named type from a different module, or with no symbol at all | reports — a wrong or unresolved domain type is still wrong |
| the oracle could not type it at all (`undefined`) | silent |

A union is judged member-wise, decided on its own terms rather than falling into the nominal
rows above: `amount: Decimal | undefined` is optional money, and no member of the union is a
forbidden primitive, so it stays silent; `amount: number | Decimal` can still be a bare `number`
at run time, so it reports.

A local `class Decimal {}` sharing the required name is still reported: `require` is matched on
where a symbol came from, so a type that merely looks right is not accepted as if it were
imported — a local declaration has no module at all, and cannot match one. A type the oracle
cannot attribute to any symbol at all — an ambient or global type such as `Date`, used with no
local declaration or import — is reported on the same terms: a governed value whose type cannot be
established is not evidence the convention is met.

### `require` is matched on the module and nothing else

`require.name` is not compared against the type. `import { Big } from 'decimal.js'` satisfies a
convention requiring `Decimal`, because it came from the required module — a false negative, and a
deliberate one.

The alternative is worse. The type oracle reports a type's name as it is written *at the use site*,
not as the module exported it, so comparing that name rejects an alias of exactly the required
type: `import { Decimal as Money } from 'decimal.js'` is conforming code, and a rule that compared
names reported it with a message about `number`. **A rule that accuses conforming code is the one
failure this design forbids**, and the whole posture of the type oracle is the same trade — say
nothing rather than say something wrong. Matching the module alone is the version of the check that
cannot produce that failure.

Two consequences to hold. Enabling this rule against a module that exports several types treats
them as interchangeable, so it is worth pointing `require.module` at the narrowest module that
exports the type you mean. And `require.name` is still load-bearing for the *message* — with no
`reason` set it is what the violation says to use instead — so it is worth spelling correctly even
though nothing checks it.

**`undefined` produces false negatives and never false positives.** The oracle would rather say
nothing than accuse code it could not read, so a value it cannot type is never reported — even
when the name matches and the value really is a raw `number`. That silence is bounded by what the
oracle can see from the parsed file alone: no `tsconfig.json`, no declaration files, no cross-file
resolution. "No violations" from this rule is a narrower claim than "every governed value
conforms," and a reader who conflates the two is trusting a report that never looked.

### It is one half of a pair

`no-restricted-types` catches a *named* value — a parameter or a declarator whose name the
convention governs. It cannot catch `new Decimal(parseFloat(row.amount))`, where nothing is
named at all. `lanekeep/no-restricted-arguments`, below, selects by callee instead and catches
exactly that shape. Neither rule subsumes the other, and a project enforcing a convention like
the money one above usually wants both.

---

## `lanekeep/no-restricted-arguments`

Forbid a primitive type in an argument position a convention governs.

The second built-in where a *type* decides the violation, and the one that reaches a call site.
The convention is the same one `no-restricted-types` exists for — "every monetary value is a
`Decimal` from `decimal.js`, and never a `number`" — but the shape is different:
`new Decimal(parseFloat(row.amount))` contains no name for a naming convention to govern. This
rule selects the other way, by the callee a call resolves to and the position an argument sits
in, and asks the type oracle what the value in that position is.

A **factory**, for the same reason `no-restricted-imports` and `no-restricted-types` are: the
restriction is the entire content of the rule, so an empty `restrictions` list reports nothing at
all. It matches a `new` expression or a plain call whose callee is a bare identifier —
`new Decimal(...)` and `Decimal(...)` are both candidates.

```json
{
  "rules": [
    {
      "rule": "lanekeep/no-restricted-arguments",
      "options": {
        "restrictions": [
          {
            "call": { "module": "decimal.js", "name": "Decimal" },
            "forbid": ["number"],
            "reason": "construct a Decimal from a string, not a float"
          }
        ]
      }
    }
  ]
}
```

Or from a `lanekeep.config.ts`:

```ts
import noRestrictedArguments from 'lanekeep/no-restricted-arguments'

export default defineConfig({
  rules: [
    noRestrictedArguments({
      restrictions: [
        {
          call: { module: 'decimal.js', name: 'Decimal' },
          forbid: ['number'],
          reason: 'construct a Decimal from a string, not a float',
        },
      ],
    }),
  ],
})
```

Its card: **message** `restricted type on an argument the convention governs`, **remediation**
"convert it before the call, or pass a value the callee is meant to take", with the example pair
`new Decimal(parseFloat(row.amount))` (reported) and `new Decimal(row.amount)` (fine). The
violation is anchored at the argument, not at the callee — the argument is the thing to change.

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `restrictions` | `Restriction[]` | What is governed. Defaults to `[]`. |

Each restriction:

| Field | Type | Meaning |
| --- | --- | --- |
| `call` | `{ module: string, name?: string }` | The callee, resolved through the import that bound it. `module` is matched exactly, not as a glob. `name` is optional: omit it to govern every export of `module`. |
| `argument` | `number \| 'all'` | Which argument is governed. Defaults to `0`. `'all'` checks every position and reports the first forbidden one. |
| `forbid` | `string[]` | Primitive type names that are a violation in that position — `number`, `string`, `boolean`, `bigint`, `symbol`, `null` or `undefined`, the set the type oracle itself recognizes. |
| `reason` | `string` | What to tell the reader. Carried into the violation message; falls back to a generic line when it is absent. |

A restriction naming no `call` governs nothing and is skipped, and a restriction with no `forbid`
list forbids nothing rather than everything — an absent list is an empty one.

### It selects by callee, which is what the name-based sibling cannot do

This is the rule that catches `new Decimal(parseFloat(row.amount))`. Nothing there is named, so
`no-restricted-types` has no candidate to govern; here the candidate is a position, and the
oracle types `parseFloat(...)` as `number`.

**The callee is matched through the import that bound it, not by the text at the call site.**
`import { Decimal as Money } from 'decimal.js'` followed by `new Money(parseFloat(x))` is
reported, because the check follows the binding — which is the question `no-restricted-types`
cannot ask at all, since the oracle reports a type's name as the use site spells it. `name` is
the export's own name: `default` for a default import, `*` for a namespace import, and omitted
to mean "anything from this module".

### The default is the first argument, and that is a deliberate narrowing

`new Decimal(parseFloat(a), 10)` is the case that settles it. The `10` is a radix literal, it
types as `number`, and a rule that checked every argument by default would accuse it alongside
`parseFloat(a)`. So position `0` is the default, and a convention governing a different position
says so with `argument: 1` — or with `argument: 'all'` when it genuinely means every one.

The cost is a silence: **until `argument` is written, a forbidden type anywhere but position 0
is not looked at.** `new Decimal(row.amount, 10)` reports nothing under the default and reports
at the `10` under `argument: 'all'` or `argument: 1`. A call site reports at most once either
way — `'all'` stops at the first forbidden position rather than listing them all.

### It judges the immediate type and does not follow a value backwards

`new Decimal(String(parseFloat(s)))` passes a restriction forbidding `number`. The argument is a
`string`; the precision died one call earlier, inside an expression this rule does not walk. That
is the boundary between a type check and dataflow analysis, not a defect — following a value back
through arbitrary conversions is a different tool.

What it does follow is one step of naming, because the oracle does: `const amount =
parseFloat(row.amount); return new Decimal(amount)` is reported, since `amount` resolves to its
declarator and the declarator's initializer types as `number`. The named form and the inline form
of the same mistake are both caught; the *converted* form is not, and is not meant to be.

### What it stays silent on

Same posture as its sibling, for the same reason: false negatives are the price, and a false
positive is the one failure this design forbids.

| The code | What happens |
| --- | --- |
| `new Decimal(row.amount)` | silent — the oracle cannot type a member expression |
| `new Decimal(...xs)` | silent — a spread element types as `undefined` |
| `new Decimal()` | silent — there is no argument in the governed position |
| `new pkg.Decimal(parseFloat(x))` | silent — the query matches a bare identifier callee, and nothing else |
| `new Other(parseFloat(x))` | silent — `Other` does not resolve to the restricted import |
| a `Decimal`-typed argument | silent — a nominal type is never a violation here; there is no `require` in this rule's shape, only `forbid` |
| `v: Decimal \| undefined` | silent — no member of the union is a forbidden primitive |
| `v: number \| Decimal` | reported — a bare `number` is still reachable through the union |

The rule declares `requires: ['types']`, which is what puts `ctx.types` on its context at all —
see [`architecture.md`](architecture.md) §6.10. Everything it can say is bounded by what the
oracle can see from the parsed file alone: no `tsconfig.json`, no declaration files, no
cross-file resolution. A clean run means the governed positions this rule could type were fine,
which is a narrower claim than "no forbidden value reaches that callee".

### It is one half of a pair

`no-restricted-types` catches a *named* value — a parameter or a declarator whose name the
convention governs — and cannot see `new Decimal(parseFloat(row.amount))`, where nothing is
named. This rule selects by callee and catches exactly that shape, and in return says nothing
about a `function credit(amount: number)` that never calls anything. Neither is a subset of the
other, and a project enforcing a convention like the money one usually configures both, from the
same `forbid` list.

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
except BaseException:      # reported — broader still: Ctrl-C and SystemExit too
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

**Both are components**, like the two Rust rules, so name them from
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

### Aliases resolve by import, not by spelling

`ctx.resolvesToImport` asks whether a name binds to a given module, so the qualifier's local
spelling is irrelevant in both directions:

```go
import ctxpkg "context"                     // ctxpkg.Context in a struct is reported — it is the standard library
import context "example.com/app/context"    // fine — a different package, whatever it calls itself
```

An earlier version of this rule matched the qualifier by name and reported the second case
too. Both directions are pinned in `crates/lanekeep-rules/tests/no_context_in_struct.rs` —
including a case whose name still says `_is_also_reported` while asserting the opposite,
kept as the historical marker of the fix.

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

**Both are components**, like the two Go rules above, so name them from a
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
default. `allow` takes patterns to widen that, matched against the wildcard's *full* text —
`super::*`, not `super`, for `use super::*;`. `*` is a wildcard here, not a literal character —
`super::*` also matches `use super::inner::*;`, and there is no escape for a literal `*`, so an
exact "only this one path" match is not expressible.

The rule is a WebAssembly component, so its options cross as data: name it in a
`lanekeep.json`, bare to keep the default `allow` — which is what `lanekeep init` writes into
a Rust project — or configured to widen it. There is no function to call and nothing to
import; a TypeScript config that tries is refused at load with
`` `lanekeep/no-glob-import` is a rule component; name it in a `lanekeep.json` ``.

```json
"lanekeep/no-glob-import"
```

```json
{ "rule": "lanekeep/no-glob-import", "options": { "allow": ["*prelude*", "super::*"] } }
```

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

The rule is a WebAssembly component, so its options cross as data: name it in a
`lanekeep.json`, bare to keep the default, empty `allow`, or configured. There is no function
to call and nothing to import; a TypeScript config that tries is refused at load with
`` `lanekeep/no-unwrap` is a rule component; name it in a `lanekeep.json` ``.

```json
"lanekeep/no-unwrap"
```

```json
{ "rule": "lanekeep/no-unwrap", "options": { "allow": ["src/main.rs"] } }
```

### What it cannot tell apart

A method genuinely named `expect` on your own type — a mock builder, say — is reported like
`Result::expect`. Telling them apart needs type information, which lanekeep deliberately does
not have (§1 non-goals). A test pins the behavior so it is a known limit rather than a surprise.

---

# Every language

## `lanekeep/no-assertionless-test`

A test that asserts nothing passes forever and covers nothing.

Agents pad coverage on request — a test body that calls the subject and checks nothing is the
cheapest way to make a coverage number move — so this fires often and early in agent-written
code. `jest/expect-expect` covers JavaScript; nothing covers Go, Python or Rust, and the
multi-language form is what earns this a built-in slot: one rule id, one suppression
vocabulary, one card, across every supported language.

It declares `['typescript', 'tsx', 'python', 'go', 'rust']`, with one query per grammar. What
is per-language is how a test is recognized and what counts as asserting:

| Language | A test is | Asserts by default |
| --- | --- | --- |
| TypeScript/TSX | an `it(...)`/`test(...)` call (`.only`/`.skip` forms included) with a block-bodied callback | `expect*`, `assert*` |
| Python | a `def test*` function, methods included | the `assert` statement, `self.assert*`, `self.fail`, `pytest.raises` |
| Go | `func Test*` taking `*testing.T` | `t.Error*`, `t.Fatal*`, `t.Fail*`, `assert.*`, `require.*` |
| Rust | a `fn` under `#[test]`, or an attribute path ending `::test` (`#[tokio::test]`) | `assert*!`, `debug_assert*!`, `panic!` |

Two exemptions are correctness rather than convenience: a Go test that calls `t.Skip*` and a
Rust test under `#[should_panic]` legitimately assert nothing and are never reported.

```json
{
  "rules": [
    {
      "rule": "lanekeep/no-assertionless-test",
      "options": {
        "tests": ["src/**", "tests/**"],
        "assertions": { "go": ["suite."] },
        "allowHelpers": ["expectValidResponse"]
      }
    }
  ]
}
```

This rule is a **factory** on the same terms as `no-restricted-calls`: a `lanekeep.config.ts`
calls it, and both a bare JSON reference and an uncalled TypeScript reference fail to load
with ``missing `id` ``. Unlike its neighbors, it is useful *without* options — every default
above applies when it is configured with `{}`.

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `tests` | `string[]` | Path globs that gate where the rule looks. Omitted means everywhere. |
| `assertions` | `{ [language]: string[] }` | Per-language additions to the default vocabulary. |
| `allowHelpers` | `string[]` | Names that count as asserting in every language. |

Vocabulary entries are matched as **prefixes** of the normalized callee — whitespace stripped,
`?.` folded to `.` — so `t.Error` covers `t.Errorf` and `self.assert` covers every
`self.assert*` method. `tests` is the one gate a multi-token judgment can have, and it is only
set when given: Rust unit tests conventionally live inline in `src/*.rs`, so a default test
glob would silently exclude them, and a wrong gate is worse than none.

### What it cannot tell apart

The rule does not chase helpers: an assertion inside a function the test calls is invisible,
which is the same limit `expect-expect` has — name such helpers in `allowHelpers` and they
count as asserting. Go's receiver is matched by its conventional name, so a
`func TestX(tt *testing.T)` asserting through `tt.Error` needs `assertions: { "go": ["tt."] }`.
A Rust `#[cfg(test)]` attribute gates compilation and does not make a function a test; only
`#[test]` and `::test` attribute paths do.

---

## `lanekeep/no-restricted-calls`

Forbid calling given callables, optionally only from given paths.

The call-expression sibling of `no-restricted-imports`: "no `console.*` outside the logging
layer", "no raw `fetch` outside the API client" — each is one restriction entry instead of a
bespoke local rule. The restrictions themselves stay project-specific (they are the options);
the mechanism — *where* a call may happen — is what is general.

It declares `['typescript', 'tsx', 'python', 'go', 'rust']`, with one query per grammar. The
restriction grammar, the `from` carve-outs and the raw-text matching are identical in every
language; what is per-language is which nodes are a call and how a qualified callee is spelled:

| Language | Qualified callee shape | Example restrictions |
| --- | --- | --- |
| TypeScript/TSX | member access | `console.*`, `fetch` |
| Python | attribute access | `requests.*`, `open` |
| Go | selector | `fmt.*`, `panic` |
| Rust | path or method | `std::fs::*`, `*.fetch` |

Its card: **message** `restricted call`, **remediation** "call something permitted here, or
move this code where it is allowed", with the example pair `console.log(metrics)` (reported)
and `log(metrics)` (fine).

The restriction is the entire content of the rule, so naming it without options does nothing
useful — give it `restrictions`:

```json
{
  "rules": [
    {
      "rule": "lanekeep/no-restricted-calls",
      "options": {
        "restrictions": [
          {
            "call": "console.*",
            "from": ["!src/logging/**"],
            "reason": "route it through the logger"
          },
          { "call": "fetch", "reason": "use the API client" }
        ]
      }
    }
  ]
}
```

This rule is a **factory** — a function you call with options, which returns a rule — so a
`lanekeep.config.ts` calls it rather than naming it:

```ts
import noRestrictedCalls from 'lanekeep/no-restricted-calls'

export default defineConfig({
  rules: [noRestrictedCalls({ restrictions: [{ call: 'console.*' }] })],
})
```

**Two spellings fail to load.** A bare `"lanekeep/no-restricted-calls"` in a JSON config, and an
uncalled `noRestrictedCalls` reference in a TypeScript config (`rules: [noRestrictedCalls]`
rather than `rules: [noRestrictedCalls({...})]`), both fail with ``missing `id` `` — a factory
function has no `id` of its own; only the rule it returns does.

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `restrictions` | `Restriction[]` | What is forbidden. Defaults to `[]`. |

Each restriction:

| Field | Type | Meaning |
| --- | --- | --- |
| `call` | `string` | The callee to forbid. `*` matches anything, including `.`. |
| `from` | `string[]` | Where the restriction applies. Omitted means everywhere. |
| `reason` | `string` | What to do instead. Carried into the violation message. |

`call` is matched against the callee **as written**, with whitespace stripped and `?.` folded to
`.` — so `console\n  .log` and `console?.log` both match `console.*`. Matching the raw text
rather than a resolved name is deliberate: a restriction is written against what an author
types, on the same terms as `no-restricted-imports`. Only the callee is normalized, so write a
restriction in dotted form (`console.*`, not `console?. *`).

### What it cannot tell apart

A call reached through computed member access — `console['log']('x')` or `obj[key]('x')` — is
not captured by the query, and a `new` expression is not a call. Telling them apart is a
different query rather than a normalization fix; a test pins the boundary so it stays a known
limit rather than a surprise.

The same boundary per language: a rust `macro_invocation` is not a `call_expression`, so
`println!` cannot be restricted by this rule — restricting macros is out of scope, and a test
pins it. Go's `go f()` and `defer f()` wrap an ordinary call, so the inner call still matches;
that is covered rather than a gap.

An entry in `from` beginning with `!` is a carve-out — the restriction applies everywhere
*except* there, and a carve-out beats an inclusion. That inversion is what makes "no `fetch`
outside the API client" one restriction rather than an enumeration of every other directory,
which would rot the first time someone adds one.

`reason` is the field worth spending time on. An agent reading lanekeep's output needs to know
what to do instead, not merely that something is banned — the reason is the part of the message
that tells it.

The violation names the callee and the reason — `calling 'console.log' is restricted — route it
through the logger` — and is anchored at the call. One violation per call, even when several
restrictions match it; the first matching restriction's reason is the one carried.

---

## `lanekeep/duplicate-implementation`

Two function bodies with the same shape — identical structure once identifiers and literal
values are erased — are one implementation written twice. This is the failure no per-file rule
can see: an agent that cannot view the whole corpus reimplements a helper that already exists,
and each individual file looks fine. It is also the rule that most directly shows why the
corpus-view architecture exists.

It declares `['typescript', 'tsx', 'python', 'go', 'rust']`, with one query per grammar — the
fingerprint itself is language-agnostic, a fold over node kinds with token text erased, so the
per-language part is only which nodes count as a function. Grouping never crosses a language:
the same algorithm in Python and in Go has different interior node kinds (`attribute` against
`selector_expression`), so the two bodies cannot share a fingerprint.

```json
{
  "rules": [
    {
      "rule": "lanekeep/duplicate-implementation",
      "options": { "minNodes": 60 }
    }
  ]
}
```

### Options

| Field | Type | Meaning |
| --- | --- | --- |
| `minNodes` | `number` | Smallest body, in fingerprint nodes, that participates. Defaults to `40`. |

The default is calibrated to fire on real helpers and stay quiet on the two-line pairs every
codebase has. Raise it if a codebase has many small repeated shapes; lower it to catch smaller
duplicates at the cost of more noise. Fingerprint nodes count kinds, anonymous tokens and
structure, so the same threshold reads slightly differently per grammar — a Python body reaches
40 a line or two later than a TSX one.

### What counts as a duplicate

Bodies are matched by structure, not by text. The fingerprint erases identifier names, literal
values and comments, so:

- Same shape with different names, values or comments → **flagged** (that is the point).
- A changed operator or an added statement → **not flagged**.
- A doc-comment difference → **flagged**: comments are erased like any other. In Python a
  docstring is a *statement*, not a comment, so the two directions pull apart — two bodies
  whose docstrings merely differ still group (the string's text is erased), while a
  with-docstring body against a without-docstring one differs by a statement and does not.
- Two identical bodies in one file → flagged, exactly like two in different files.

What participates, per language: function declarations, methods, function expressions and
block-bodied arrow functions in TypeScript/TSX (expression-bodied arrows do not, and generator
declarations and expressions are not covered in v1); `def` functions and methods in Python
(lambdas do not); functions and methods in Go; `fn` items in Rust (closures do not). Within a
language the fingerprint is rooted at the body, so a method whose body matches a function's is
flagged like any other pair.

### Full runs only

Like every cross-file rule, this one is skipped under `--since` and `--staged` with a stderr
notice naming it ([`architecture.md`](architecture.md) §8.4) — it reports on full runs only.

---

## Composing them

The two cross-file rules share their module resolution, which is exported as
`lanekeep/paths` and available to project rules too, and `no-restricted-imports` and
`no-restricted-calls` share their pattern matching — the `*` globs and `!` carve-outs — as
`lanekeep/patterns`:

```ts
import { resolveImport, dirname, join } from 'lanekeep/paths'
import { appliesTo, matches } from 'lanekeep/patterns'
```

Two rules resolving `./a` differently would not look like a bug — each would be individually
plausible — so the resolution has one definition. The same holds for a `*` glob or a `from`
list: two rules interpreting one differently would each be individually plausible.

---

## Adding one

A built-in gets no privileged path into the engine, which is the point: a built-in that
needed something a project rule cannot have would be evidence the host API is wrong.

1. Write `crates/lanekeep-rules/rules/<name>.ts`, importing only from `lanekeep` and the shared
   modules under the same prefix (such as `lanekeep/patterns`).
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
things about TinyGo that are not preferences — four of which fail silently, and two of which
stop the build and say so. Step 3 uses `RuleTester::for_built_in` rather than `for_component`,
for the reason the note below gives about a shared artifact.

There is no component step for a TypeScript built-in. The four TypeScript-inspecting rules
shipped as one compiled component for a release cycle and were reverted to QuickJS modules by
measurement — architecture §16 M5 tells that story, and §5.2's compiled form remains available
to a project that builds a component itself; nothing compiles one on demand. A TypeScript built-in goes in `BUILT_IN_RULES`, never `COMPONENT_RULES`,
and its `RuleTester` suite runs the same source the binary embeds, so there is no second
artifact left for the tests to miss.

Cover the forms that do not look like the obvious one. Every gap found while writing these
two rules was a form that read differently in source but meant the same thing.
