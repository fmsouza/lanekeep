# Type-aware rules

## Writing a type-aware rule

A rule that needs to know what a value *is* rather than what it is spelled. This is the
playbook for `requires: ['types']`; [`architecture.md`](architecture.md) §6.10 is the
contract, and this is how to work within it.

### Declare the capability

```ts
export default defineRule({
  id: 'acme/no-raw-money',
  requires: ['types'],
  query: { typescript: '(required_parameter pattern: (identifier) @name)' },
  card: { /* … */ },
  check(ctx, m) {
    const type = ctx.types.typeOf(m.name)
    if (type?.primitive !== 'number') return
    ctx.report(m.name)
  },
})
```

`requires: ['types']` is what puts `ctx.types` on the context at all. A rule that forgets it
still compiles — TypeScript has no way to see that a rule's own `requires` is what makes the
namespace exist — and finds out at the first call, where `ctx.types` is `undefined` and
`ctx.types.typeOf(...)` throws a `TypeError`. That loudness is deliberate: the alternative is a
rule that reports nothing and reads as a clean codebase.

## The five questions

| Ask | When |
| --- | --- |
| `typeOf(n)` | "Is this a `number`?" — a primitive, a union of them, or a named type with a symbol |
| `symbolOf(n)` | "Did this come from that package?" — `module` is the specifier as written, `exported` the name that module declares it under |
| `returnTypeOf(n)` | "What does calling this give back?" — a call expression, a function-like declaration, or an identifier bound to one |
| `isAssignableTo(n, module, name)` | "Is this that library's type, or something that extends it?" |
| `complete()` | "Did I see everything this file imports?" |

`returnTypeOf` is separate from `typeOf` because a function declaration is not an expression.
Folding it in would have meant a function-type variant every rule asking a simpler question
would then have to unpack, for one question rules actually ask.

`symbolOf`'s two name fields are not interchangeable. `name` is the spelling at the *use site*
— `import { Decimal as Money }` gives `Money` — and is what a message quotes. `exported` is
what the declaring module calls it, which is what a rule compares against a required export
name. Comparing `name` rejects a renamed import of exactly the right type, which is a false
positive on conforming code.

## The silence posture

**Every question can answer `undefined`, and that is a result rather than a failure.** The
provider would rather say nothing than say something wrong, because a rule reporting on a wrong
type accuses correct code. A rule is expected to check for `undefined` and stay silent.

`isAssignableTo` is where the distinction is sharpest, because it has three answers:

| Answer | Means |
| --- | --- |
| `true` | the type is that export, or reaches it through `extends` |
| `false` | the walk completed and reached nothing |
| `undefined` | a link could not be read — an unresolvable import, a package that is not installed, a type with no symbol at all |

A rule writing `if (!ctx.types.isAssignableTo(n, m, t)) report()` reports on every file whose
`node_modules` is absent. Write the three arms.

## What stays `undefined`

Under the builtin provider, always:

- **Generic instantiation.** `useQuery<Balance[]>` answers by the result type's *name*; the
  type argument is dropped everywhere in this crate.
- **Conditional, mapped, function and object types.** Each would need an abstraction the
  bounded oracle does not have, and guessing is worse than silence.
- **Declaration merging**, and ambient `declare module` blocks as a resolution source.
- **`.tsx` files reached through an import**, project sources included. The provider parses
  everything it opens with the TypeScript grammar, under which every JSX element is an `ERROR`
  node with nothing reported anywhere, so a file it cannot read honestly is one it does not
  read. The grammar is selected by name rather than by taking whichever registered language
  answers first — `tsx` sorts before `typescript`, and picking it would parse every `.ts` file
  the provider opens with the wrong dialect. A declaration file is never TSX, so nothing is
  lost in `node_modules` — but a **project source** is another matter: `import { Button } from
  './Button'` with `Button.tsx` beside it resolves to nothing, records six absent reads, and
  makes the importing file `complete() === false`. On a React codebase that is most sibling
  imports, so a rule there should expect `complete()` to answer `false` far more often than
  the missing-`node_modules` case suggests. A stated limitation of this release; the
  refinement is filed with the resolver's own issue.
- **A destructured binding.** `function f({ amount }: Money)` types `amount` as nothing —
  reading the pattern's own annotation would hand every name the whole thing's type.
- **A type parameter.** `interface O<T> { x: T }` types `x` as nothing, because `T` is whatever
  the call site chose.

And, situationally: anything behind an import that did not resolve. `complete()` is how a rule
finds out that happened — with two deliberate exclusions. An import that resolves to a file
which does not *parse* counts as unread, because the names outside the broken span answer while
the ones inside it come back `undefined` and nothing on either answer says which. And an import
of something that is not code — `./app.css`, `./data.json`, `./logo.svg` — is not counted at
all: it is not a module the oracle reads, and counting it would label most of a bundler's
project incomplete for having stylesheets. A specifier is skipped only when its last segment
carries a known asset extension, never when it merely looks like one: `./user.service`,
`./auth.guard` and the rest of the NestJS and Angular vocabulary are modules and are probed.

**The verdict is the whole file's, and a parse fault is the whole declaration file's.** One
`ERROR` node anywhere in a fifty-thousand-line `@types` bundle makes every file that imports it
`complete() === false`, however far that span is from the names the rule asked about. Silence is
the safe direction — a rule told the view is partial stays quiet, where a narrower verdict that
was wrong would let it report — so the coarse answer is what this release gives; an `ERROR`
covering the *asked* name is a refinement filed with the resolver's own issue.

Two further limits, specific to individual questions:

- **An alias chain cut by the depth bound answers nothing**, never a type from an intermediate
  file in the chain.
- **`returnTypeOf` reads a declared return annotation or, absent one, the body's `return`
  expressions; an `async` function or generator with no annotation answers nothing.**

And two specific to `symbolOf` and `isAssignableTo`:

- **A relative import re-exported through an intermediate file reports that intermediate
  file's own specifier in `symbol.module`**, not the original module the value came from — a
  bare package name, by contrast, is reported exactly as written at the use site.
- **`isAssignableTo` is nominal**: `extends`/`implements` and aliases of the named type,
  followed across files. A same-named local declaration is never the target regardless of
  shape, declaration merging is not followed, a generic annotation at the use site answers
  nothing, a target the requested module does not export answers nothing, and a function-local
  shadow of the name is looked up at the top level rather than in its own scope.

## What it reads, and confinement

The file under check, and the declaration files its imports resolve to:

1. A **relative** specifier tries `x.ts`, `x.mts`, `x.cts`, `x.d.ts`, `x/index.ts`,
   `x/index.d.ts`, in that order. A specifier naming the emitted JavaScript — `./money.js`,
   TypeScript's own ESM spelling — is tried at the same stem.
2. A **bare** specifier walks `node_modules` upward from the importing file's directory. In a
   package it reads `package.json`: `exports` under the `types` condition (subpath maps and
   `*` patterns included), then `types`, then `typings`, then `index.d.ts`; then the same for
   `@types/<name>`, with a scoped package flattened as `@types/scope__name`.

Every one of those probes is a **tracked read**, hit or miss, so an absent file is recorded
with a null hash and its later appearance recomputes exactly the files that noticed it was
missing.

**The walk stops at the project root, and nothing above it is ever read.** The two ways past
the root fail differently, and both are unresolvable rather than an error:

- A `node_modules` **hoisted above the root** is never probed at all — the walk stops, so no
  path above the root is read and none is recorded.
- A package **symlinked out of the root**, which is what a pnpm store is, *is* probed: the path
  names something inside the root. The read is refused after canonicalizing and recorded as a
  **refused** dependency — a third outcome distinct from absence, because the validator
  re-resolves it under the same confinement rather than looking for the file — so the day that
  path becomes a real in-root file every answer that rested on the refusal is recomputed rather
  than served from the cache forever.

For the hoisted case nothing is recorded at all, since the walk never reaches it; for the
symlinked-out case a refusal is recorded, not an absence. Either way every answer that needed
the package is `undefined`, and `complete()` is `false`. **The remedy is to point lanekeep at
the workspace root** — the directory `node_modules` lives in — rather than at a package inside
it. `--config` does not move the root.

## What it costs

Declaration files are parsed once per run, whatever imports them, and cached for the run.

**Project sources reached through an import are parsed a second time**, by the provider's own
parser: a relative specifier prefers `x.ts` over `x.d.ts`, so importing a sibling source means
the engine parses it for its own check and the provider parses it again to answer about it.
Once per run per file rather than once per importer, so the bill is the number of distinct
files reached through imports, not the number of imports — with one exception at the margin:
the provider's parser is behind a plain mutex held across a whole parse, so two workers that
reach the same uncached file at the same moment both parse it and the second write wins. At
most one extra parse per worker that races, of the same bytes, for the same answer. Sharing the engine's node arena would
remove it and is a separate seam — the arena is keyed by the run's file list, and a `.d.ts`
under `node_modules` is not in it at all.

The cost that shows up in a cache is the *dependency list*: a file importing three packages
records every probe those three took, and a package that resolves on its first candidate
records one read rather than six.

**Measured**, 2026-09-08, on a `pera-corpus` clone at
`3b17bb2ed15e4fcd113b962b2ab26e2347b22dcd` — 6,556 files checked, one `requires: ['types']`
rule configured (`lanekeep/no-restricted-types` with the `Decimal` money convention from
[`built-in-rules.md`](built-in-rules.md), passed as `--config`), each binary a release build
run cold with `.lanekeep/` removed first, on an Apple M3 Max (14 cores), macOS 26.6.2:

| binary | `.lanekeep/cache` | violations | wall clock |
| --- | --- | --- | --- |
| `7416b00`, the plan-3 base — the within-file oracle | 378,092 bytes | 107 | 1.70 s, 1.73 s |
| `639288b`, this branch — the cross-file oracle | 438,015 bytes | 110 | 1.74 s, 1.67 s |

**+59,923 bytes, about 9 bytes per file checked, +15.8%** — and three violations the within-file
oracle could not reach, which is what the bytes bought. The byte and violation figures
reproduced exactly on a second cold run of each binary; the two wall-clock figures are both
runs rather than an average, because at this size the spread between them is larger than the
difference between the binaries. This is the cost *when the oracle is used*; task 17 measured the cost when it is
not, over the same corpus with the project's own config, and found a **0-byte delta**, because
a run that asks the oracle nothing opens no declaration file and records no probe.

`639288b` is the commit *below* this file's own — the docs commit adds prose and doc comments
only, so it compiles to the same behavior and measuring at it would measure the same run.

Two earlier versions of this table named SHAs this history no longer has — `b448677`, a
pre-rebase working commit, and then `dd8e563`, which a later rebase rewrote into `639288b`.
Neither row was reproducible by anyone, which is why the whole table is re-measured at a live
SHA rather than carried forward whenever the commit under it moves.

What survived every re-measurement is the pair the argument rests on: **378,092 / 107 and
438,015 / 110 came back byte-identical at `b448677`, at `dd8e563` and at `639288b`**, over the
corpus at the SHA named above. The file count moved once, 6,557 to 6,556, and this method gives
6,556 on every run since — nothing here explains the earlier figure, so read the count as this
row's rather than as a constant of the corpus.

The wall clock is the column that did **not** reproduce, and it is reported rather than
smoothed. The `dd8e563` run recorded 0.90 s / 1.06 s for the base and 1.21 s / 1.04 s for the
branch; the `639288b` run above, same machine and same method, gives 1.70 s / 1.73 s and
1.74 s / 1.67 s — every figure slower, and the two binaries now indistinguishable from each
other. Nothing in the code between those two commits could account for it (the rebase moved
none of the engine), so what the two runs really measure is the machine at two moments. That
is the column's own caveat arriving in earnest: at this size the spread between runs is larger
than the difference between the binaries, and only the byte and violation figures should be
read as properties of the change. The sentence that once read "about three seconds" named
neither a machine nor a run and is replaced by the column.

One thing the numbers do not cover: neither run's config resolves a package outside the corpus,
so a project whose imports reach deep into `node_modules` records more probes per file than
this.

<!-- Do not rewrite a recorded figure — if a later measurement disagrees, add it beside this
     one with its own SHA and date. A figure measured against a working tree rather than a
     named commit is reproducible by nobody who lacks that tree. -->

## Rules that ship with this

`lanekeep/no-restricted-types` and `lanekeep/no-restricted-arguments`; see
[`built-in-rules.md`](built-in-rules.md) for what each stays silent on.

## Providers

Which oracle answers `ctx.types` is a project-level setting, not something a rule says. A rule
declares `requires: ['types']` and asks its question; the config decides who answers.

```jsonc
"types": {
  "provider": "builtin",                    // the default
  "command": ["node"],                      // how to launch the sidecar; tsc only
  "typescript": "./node_modules/typescript" // the package it loads; tsc only
}
```

**Each of the two `tsc` values is resolved differently, and neither against the directory
lanekeep was invoked from.** `command[0]` is a program name, found by the operating system on
`PATH` — it is never joined to the project root, so `node` means whichever `node` a shell in
that project would run. `typescript` is resolved the way Node resolves a `require` from the
project root's own `package.json`: `./node_modules/typescript` is that project's copy, and a
bare name is a package looked up through that project's `node_modules` rather than through
lanekeep's. The project root itself is made absolute once, before the sidecar is started, so
`lanekeep check .` and `lanekeep check ../app` mean what they say.

### `builtin`

lanekeep's own oracle, over the files it parsed. No toolchain, no process, nothing to install,
and it runs under the ordinary run budget without coming near it. It resolves imports, walks
declaration files and answers by name. What it does not compute is listed in the section above
and is worth reading before choosing the other one: generic instantiation, conditional and
mapped types, and declaration merging all answer `undefined`.

### `tsc`

The project's own `typescript` package, loaded by a driver script lanekeep writes into
`.lanekeep/` and runs with the project's own Node. It answers everything the compiler answers,
and it costs what the compiler costs.

**Which files it answers for.** TypeScript, TSX, JavaScript and JSX — `.ts`, `.mts`, `.cts`,
`.d.ts`, `.tsx`, `.js`, `.mjs`, `.cjs` and `.jsx`. A `.js` file is typed under the project's own
`allowJs`, so a rule declaring `requires: ['types']` for `javascript` gets `ctx.types` under
this provider and not under `builtin`, which reads a grammar lanekeep parses itself and has no
type annotations to read there.

**It builds the project's programs before it checks anything**, at prepare, so that the run key
can be computed before any file is checked. What it folds is not "the program's source files":
the driver records every file the compiler host was asked to read while building that program
and answering every question put to it — `tsconfig.json` and its `extends` chain, every
`package.json` module resolution consulted, and every source and declaration file — each with
the hash of the bytes read, listed relative to the realpathed project root, minus the
`typescript` package's own directory, whose version is covered separately (architecture §8.1).
A pre-commit hook using it is a slow hook, and lanekeep says so on stderr every time.

**The project root bounds which `tsconfig.json` is used, and a file no config under it claims
is typed without one.** The driver walks up from a file looking for a `tsconfig.json` and stops
at the project root: a config *above* the root would pull a program together out of files
lanekeep is not checking, and lanekeep's confinement stops at that directory. A file no config
under the root claims falls to an ad-hoc program with the driver's own options — `allowJs` and
`skipLibCheck` on, `strict` off, and nothing the project configured — so its answers depend on
where the root was pointed rather than on the project. Nothing is wrong when that happens and
the run is correct, so it is not an error; it is a line on stderr naming how many files it
happened to, and which files fell that way is part of the run key, since two programs over one
file with `strict` on and off have byte-identical listings. If you see it and did not mean it,
point lanekeep at the directory the `tsconfig.json` lives in.

**Any change anywhere recomputes every type-aware file.** That listing is this provider's whole
dependency mechanism, so one hash over the whole set, folded into the run key, invalidates
everything on any change — an edit to one file invalidates the corpus. That is deliberate: this
provider's answers are whole-program, a `.d.ts` edit anywhere can change the type of an
expression anywhere else, and a per-file dependency list would run to hundreds of paths per
cache entry the way the builtin oracle's does. Results stay correct either way; only the
recompute is bigger.

**`timeouts.analysis` bounds it**, at 60 s by default. It is not a wall-clock deadline measured
from when the run starts — it is an accumulator, charged only while the run's single sidecar is
doing the work of an answer, under that provider's session lock, so what it bounds is the
sidecar's own busy time rather than how long the run has been going. Under `types.provider:
'builtin'` nothing spends it. A breach cancels the run with exit 2, naming analysis rather than
a rule, because no rule is executing when it fires; a monorepo will need to raise it.

**A breach, or a sidecar that has died, makes every later question in that run answer the same
way.** Once the budget is spent, or the sidecar is gone, the provider keeps its first error and
answers `undefined` to everything the engine asks after — one question per remaining file, each
discarded rather than trusted. The run itself is still cancelled, with exit 2, naming
`timeouts.analysis` or whatever the sidecar's own refusal said. Files that had already completed
keep their cache entries, and a provider a future long-lived session holds across runs is
cleared at the start of the next one rather than carrying the failure forward.

If Node or the `typescript` package cannot be reached, a rule declaring `requires: ['types']`
fails the run at prepare, naming itself and the command that failed, rather than loading and
quietly answering `undefined` — which would read as a codebase with nothing to report:

```
the type provider could not be used
`acme/typed` requires the `types` analysis, which this build does not provide — the implemented capabilities are `dataflow`
  the configured `tsc` command (`definitely-not-node`) could not be started: cannot start the type provider: No such file or directory (os error 2)
  `types.provider` is `tsc`, which runs the project's own toolchain, so it needs `types.command` on PATH
```

**Which TypeScript.** The driver is written against the TypeScript 5.x compiler API
(`createProgram` and its neighbors) and measured against 5.9.3. It probes for that API in its
handshake and refuses, naming the version and the missing function, when the package does not
provide it — the reference corpus is on TypeScript 7.0.2, whose package need not, and this
provider was not measured against it. Two layouts to know about: a pnpm workspace has no root
`node_modules/typescript`, so `types.typescript` names a workspace package's copy; and a
project using `compilerOptions.paths` needs nothing from lanekeep under this provider, since
the project's own compiler resolves them — the builtin provider is the one that does not.

**Where the two providers agree, and where they do not.** Both answer the same primitives from
the same annotations. Where they part is reduction, not correctness: on a generic or a wrapped
alias the builtin oracle is silent, and `tsc` answers with the *written* type — `Box<Amount>`,
`Unwrap<Promise<Amount>>` — the same way it erases `type Amount = number` down to `number`
where the builtin oracle keeps the alias. Neither provider is wrong about code the other one
also answers; they disagree about how far a type is reduced before a rule ever sees it.

## Provider measurement

What the `tsc` provider (`types.provider: 'tsc'`, spec §5) has to pay for on a real
monorepo, measured before it was designed rather than claimed after it shipped. Every number
below is keyed to one immutable corpus commit and one machine; a figure measured against a
working tree is reproducible by nobody who lacks that tree.

The design for the type providers is posted on
[#185](https://github.com/fmsouza/lanekeep/issues/185) and kept at
<https://gist.github.com/fmsouza/cda0a0438e1d690a2bc58093f1f6ee89>; the § numbers below are
its sections.

### Reproduction

| | |
|---|---|
| Corpus | `perawallet/pera-react-native` @ **`3b17bb2ed15e4fcd113b962b2ab26e2347b22dcd`** (branch `main`, 2026-09-03) |
| Package manager | `pnpm@10.28.1`, pinned by the corpus's own `packageManager` field — **not npm**: there is no `package-lock.json`, so `npm ci` fails outright |
| Install | `corepack pnpm install --frozen-lockfile` |
| Node | `v24.18.0` |
| TypeScript | `7.0.2` — resolved per-package from the `pnpm-workspace.yaml` catalog (`typescript: ^7.0.2`); there is no root `node_modules/typescript` (pnpm's isolated linker) |
| Machine | macOS 26.6.2, Mac15,10 (Apple M3 Max, 14 cores) |
| Date | 2026-09-07 |
| Measured by | lanekeep epic #185, commit 1 |

### Program shape

The question §5.3 turns on: is there one program or many?

| | |
|---|---|
| Root `tsconfig.json` | none |
| Configs under `apps/*` | 2 |
| Configs under `packages/*` | 40 |
| Configs under `extensions/*` | 12 |
| **Total in the three globs** | **54** |
| Configs carrying `"references"` | 0 |
| Outside the globs | `conformance/tsconfig.json` — a fourth workspace root; it is now in the cost table below rather than only asserted reachable |

**Many programs, no project graph.** Nothing composes these 54 into one build, so the driver
builds one `Program` per config that contains a queried file and an ad-hoc program for a file
no config claims, exactly as §5.3 specifies. The §5.1 decision rule — a root config carrying
`"references"` would send the driver to the reference graph instead — was checked and does not
fire.

Two shapes worth naming before the driver is written, both of them out of §0's scope and both
present here: `apps/mobile/tsconfig.json` uses `compilerOptions.paths`, and every config
`extends` a package resolved from `node_modules`, so no config can be read at all before the
install completes.

### Cost per program

One `tsc` invocation per config, measured with `/usr/bin/time -p` around
`pnpm --dir <dir> exec tsc -p tsconfig.json --noEmit --emitDeclarationOnly false --declaration false`.

The three overrides are required, not tidying: 40 of the 54 configs set `emitDeclarationOnly`,
which TypeScript refuses to combine with `--noEmit`. Cold is the first run after every
`*.tsbuildinfo` was deleted; warm is the next run of the same config. A non-zero exit is a type
error in this commit of the corpus and does not invalidate the timing.

| config | files in program | cold (s) | warm (s) | exit |
|---|---|---|---|---|
| `apps/browser/tsconfig.json` | 1356 | 0.40 | 0.40 | 1 |
| `apps/mobile/tsconfig.json` | 10458 | 1.71 | 1.46 | 1 |
| `conformance/tsconfig.json` | 1201 | 0.54 | 0.50 | 1 |
| `extensions/keystore-chrome/tsconfig.json` | 337 | 0.50 | 0.33 | 1 |
| `extensions/ledger-react-native-usb/tsconfig.json` | 270 | 0.50 | 0.30 | 1 |
| `extensions/ledger-react-native/tsconfig.json` | 615 | 0.50 | 0.31 | 1 |
| `extensions/ledger-shared/tsconfig.json` | 299 | 0.47 | 0.30 | 1 |
| `extensions/ledger-web-ble/tsconfig.json` | 462 | 0.47 | 0.31 | 1 |
| `extensions/ledger-web-usb/tsconfig.json` | 273 | 0.51 | 0.30 | 1 |
| `extensions/passkey-autofill/tsconfig.json` | 323 | 0.49 | 0.30 | 1 |
| `extensions/platform-chrome/tsconfig.json` | 1069 | 0.51 | 0.34 | 1 |
| `extensions/platform-driver/tsconfig.json` | 91 | 0.46 | 0.29 | 1 |
| `extensions/platform-react-native/tsconfig.json` | 1067 | 0.53 | 0.34 | 1 |
| `extensions/platform/tsconfig.json` | 483 | 0.49 | 0.31 | 1 |
| `extensions/provider/tsconfig.json` | 559 | 0.51 | 0.34 | 1 |
| `packages/accounts/tsconfig.json` | 1615 | 0.42 | 0.41 | 1 |
| `packages/age-gate/tsconfig.json` | 159 | 0.47 | 0.29 | 1 |
| `packages/analytics/tsconfig.json` | 144 | 0.46 | 0.29 | 1 |
| `packages/app-integrity/tsconfig.json` | 287 | 0.47 | 0.30 | 1 |
| `packages/arc0027/tsconfig.json` | 147 | 0.48 | 0.31 | 0 |
| `packages/asa-inbox/tsconfig.json` | 740 | 0.51 | 0.34 | 1 |
| `packages/assets/tsconfig.json` | 859 | 0.52 | 0.35 | 1 |
| `packages/background/tsconfig.json` | 264 | 0.48 | 0.30 | 1 |
| `packages/backup/tsconfig.json` | 883 | 0.61 | 0.43 | 1 |
| `packages/banners/tsconfig.json` | 456 | 0.49 | 0.31 | 1 |
| `packages/blockchain/tsconfig.json` | 1150 | 0.63 | 0.36 | 1 |
| `packages/card/tsconfig.json` | 997 | 0.56 | 0.38 | 1 |
| `packages/config/tsconfig.json` | 334 | 0.48 | 0.31 | 1 |
| `packages/contacts/tsconfig.json` | 269 | 0.47 | 0.30 | 1 |
| `packages/currencies/tsconfig.json` | 738 | 0.60 | 0.32 | 1 |
| `packages/database/tsconfig.json` | 685 | 0.55 | 0.31 | 1 |
| `packages/dev-fixtures/tsconfig.json` | 96 | 0.47 | 0.29 | 1 |
| `packages/device/tsconfig.json` | 385 | 0.49 | 0.31 | 1 |
| `packages/fee-delegation/tsconfig.json` | 616 | 0.48 | 0.32 | 1 |
| `packages/hardware-wallet/tsconfig.json` | 153 | 0.50 | 0.30 | 1 |
| `packages/kms/tsconfig.json` | 719 | 0.51 | 0.34 | 1 |
| `packages/ledger/tsconfig.json` | 210 | 0.51 | 0.30 | 1 |
| `packages/messages/tsconfig.json` | 522 | 0.50 | 0.33 | 1 |
| `packages/migrate/tsconfig.json` | 746 | 0.51 | 0.33 | 1 |
| `packages/multisig/tsconfig.json` | 452 | 0.53 | 0.33 | 1 |
| `packages/nfd/tsconfig.json` | 869 | 0.51 | 0.32 | 1 |
| `packages/onramp/tsconfig.json` | 717 | 0.51 | 0.34 | 1 |
| `packages/passkeys/tsconfig.json` | 510 | 0.51 | 0.33 | 1 |
| `packages/polling/tsconfig.json` | 317 | 0.51 | 0.30 | 1 |
| `packages/projects/tsconfig.json` | 419 | 0.60 | 0.35 | 1 |
| `packages/remote-config/tsconfig.json` | 204 | 0.48 | 0.31 | 1 |
| `packages/search/tsconfig.json` | 190 | 0.48 | 0.32 | 1 |
| `packages/security/tsconfig.json` | 332 | 0.48 | 0.34 | 1 |
| `packages/settings/tsconfig.json` | 219 | 0.48 | 0.30 | 1 |
| `packages/shared/tsconfig.json` | 522 | 0.51 | 0.34 | 1 |
| `packages/signing/tsconfig.json` | 1474 | 0.65 | 0.47 | 1 |
| `packages/staking/tsconfig.json` | 401 | 0.50 | 0.31 | 1 |
| `packages/swaps/tsconfig.json` | 628 | 0.54 | 0.34 | 1 |
| `packages/transactions/tsconfig.json` | 918 | 0.63 | 0.42 | 1 |
| `packages/walletconnect/tsconfig.json` | 472 | 0.51 | 0.34 | 1 |
| **total (55 configs)** | **40681** | **29.19** | **19.42** | |

**What this bounds.** Summed cold across all 55 configs, `types.provider: 'tsc'` adds
29.19 s to a cold run before a single lanekeep rule executes — the figure
`timeouts.analysis`'s 60 s default (§5.2) has to be defended against or moved for. The
slowest single config, `apps/mobile/tsconfig.json`, is 1.71 s cold, about 6% of that total.
The 60 s default survives contact with this commit of the corpus: the 55-config sweep uses
49% of it, and the slowest config alone is roughly 35× under it. This table sums a
single-threaded program build, which is what the default was sized against; under parallel
queries `timeouts.analysis` grows with the sidecar's total busy time rather than with wall
clock, as the Providers section above states, so a run's rule phase adds only what the sidecar
actually spent answering, not the wall time several workers spent waiting for it.

### End to end, through `lanekeep check` itself

The table above times `tsc` directly, one config at a time. This one times `lanekeep check`
over the whole corpus (the same commit, same machine), with a single rule declaring `requires:
['types']` — a query on every `type_annotation` calling `ctx.types.typeOf` and reporting
nothing, so what is measured is the cost of asking rather than the cost of finding. Commits:
baseline `7416b00`, head `b22d280`. (`b22d280` was `70fe7b3` when these were taken; the rebase
that landed plan 5 replaced it, and it is plan 5's last *code* commit — the docs commit above it
adds prose only, so the binary these numbers describe is the one `b22d280` builds.) Machine:
macOS 26.6.2, Apple M3 Max. Full method, every
command and every deviation from the plan that produced these numbers: `.superpowers/sdd/
2026-09-07-epic-185-completion/task-5.14-measurement3-report.md`.

| configuration | wall clock (cold, s) | cache after (KB) | violations |
|---|---|---|---|
| baseline, no rules | 0.08 / 0.08 | 4 | 0 |
| head, `types.provider: 'builtin'` | 0.84 / 0.80 | 4,380 | 1 |
| head, `types.provider: 'tsc'` | 16.15 / 18.27 (warm 10.86) | 288 | 1 |

The one violation under the two type-aware configurations is `lanekeep/suppression`, not the
measurement rule (which never reports) — an existing undocumented suppression in the corpus's
own rule sources, present under every configuration that runs at least one rule.

The `tsc` provider's whole-program cost at `b22d280` (16.15/18.27, warm 10.86) sits close to the
`9f70706` measurement's (17.44/16.42, warm 11.02) — within the same run-to-run noise band this
run's own two cold runs show against each other (a ~13% spread between two runs of the identical
binary and config back to back), not a clear directional move. This is consistent with what was
anticipated going in: the driver change landed between `9f70706` and `b22d280` (as part of
`a9f97ad`, "an opt-in `tsc` provider behind `types.provider`") only skips rebuilding a program on
a *second or later* `programs` call within one run when none of its roots have moved — a root the
compiler declined (e.g. a `.js` file outside `allowJs`) is no longer compared against nothing on
every call, and a root gone from disk is dropped rather than compared forever. On a corpus run
cold each time except the warm row — and a warm row that still rebuilds every program to compute
the run key before any cache entry can be trusted, as this section already states — the code path
that change improves is not the dominant cost in either shape measured here. The `86d469a` →
`9f70706` drop (~45%, from changing `ts.createProgram` to run once per `tsconfig.json` rather than
once per file) remains the change that moved this figure; `b22d280` did not move it further. The
cache directory is smaller under `tsc` than under `builtin` (288 KB against 4,380 KB) — expected
from the Providers section above: one whole-corpus hash rather than a per-file dependency list.

`crates/lanekeep-types/src/tsc/driver.mjs`'s `programs` request, run by hand over the same
4,159 files: **10,384 rows** (unchanged from `9f70706`), **1,827,048 bytes** of listing JSON (up
from `9f70706`'s 1,806,281 despite the identical row count — not chased further, since `a9f97ad`
is a large feature commit and the figure this section tracks, the `tsc` wall clock, did not move
outside noise), **16** ad-hoc (unconfigured) files — the read-set a whole-program build folds
into one cache term.
