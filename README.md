# lanekeep

**Deterministic, AST-based architectural conformance checking for AI-generated and human-written code.**

[![crates.io](https://img.shields.io/crates/v/lanekeep-cli?label=crates.io)](https://crates.io/crates/lanekeep-cli)
[![npm](https://img.shields.io/npm/v/lanekeep?label=npm)](https://www.npmjs.com/package/lanekeep)
[![PyPI](https://img.shields.io/pypi/v/lanekeep?label=pypi)](https://pypi.org/project/lanekeep/)
[![CI](https://github.com/fmsouza/lanekeep/actions/workflows/ci.yml/badge.svg)](https://github.com/fmsouza/lanekeep/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

lanekeep enforces the conventions that live in your team's heads and your reviewers' comments —
the ones a language model cannot infer from the code it is shown. Every rule is a codified answer
to **"the agent keeps doing this wrong."**

Checks **TypeScript, JavaScript, Python and Go**. Ships as a single static binary with no runtime
dependency.

---

## Quick start

Sixty seconds, from nothing to a rule catching something.

**1. Install** — whichever fits the project you are adding it to:

```bash
npm install --save-dev lanekeep
```

<details>
<summary>Python, Go, Homebrew, cargo, or a raw binary</summary>

```bash
pip install lanekeep                                      # Python
go get -tool github.com/fmsouza/lanekeep/cmd/lanekeep     # Go
brew install fmsouza/tap/lanekeep                         # macOS / Linux, system-wide
cargo install lanekeep-cli                                # from source
```

Or download from the [releases page](https://github.com/fmsouza/lanekeep/releases).

</details>

**2. Scaffold a config and a first rule:**

```bash
npx lanekeep init
```

That writes two files, both runnable:

```
lanekeep.json                 # what to check, and with which rules
lanekeep/rules/<starter>.ts   # a worked example you can edit
```

It detects whether the project is Go, Python or TypeScript and scaffolds accordingly — the
right glob, a starter rule in that language, and a built-in worth having on.

**3. Check:**

```bash
npx lanekeep check
```

```
src/payment.ts:12:3 error [local/no-debugger] debugger statement
  → remove it before committing

✖ 1 error(s) across 1 file(s) checked
```

> **If it says `0 file(s) checked`**, nothing matched the config's `include`. The scaffold starts
> with `src/**/*.{ts,tsx}` — widen it to wherever your code actually lives.

That is the whole loop. Everything below is detail.

---

## What it is

lanekeep is not a linter in the ESLint sense. ESLint enforces language-level correctness; lanekeep
enforces *project-specific* conventions. The two do not overlap much, and lanekeep is not a
replacement for either your linter or your formatter.

Rules are TypeScript programs, written in the same language as the code they inspect:

```ts
import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-numeric-sizes',
  severity: 'error',

  card: {
    message: 'Literal numeric size inside makeStyles',
    remediation: 'Use theme.spacing.*, theme.borderRadius.* or theme.borders.*',
    examples: { bad: 'padding: 12', good: 'padding: theme.spacing.md' },
  },

  // Matched in Rust, at native speed. Your code runs only on matches.
  query: `
    (pair
      key: (property_identifier) @prop
      value: [(number) (unary_expression operand: (number))] @value) @match
  `,

  check(ctx, m) {
    if (!/^(padding|margin|gap|borderRadius)/.test(ctx.text(m.prop))) return
    if (Number(ctx.text(m.value)) === 0) return

    const call = ctx.closestAncestor(m.match, '(call_expression function: (identifier) @f)')
    if (!call) return
    if (!ctx.resolvesToImport(call.f, { module: '@rneui/themed', name: 'makeStyles' })) return

    ctx.report(m.match)
  },
})
```

`check` is ordinary TypeScript. Loop, accumulate state, build data structures, read other files,
import shared helpers — there is no expressiveness ceiling and no DSL to learn beyond the query
that gates it.

**The card is not documentation.** `message`, `remediation` and `examples` are mandatory, because
they are what gets fed back to whoever has to act on the violation — increasingly an agent.

> **Editor types are not shipped yet.** `defineRule` and `defineConfig` resolve inside lanekeep's
> sandbox at run time, so rules execute correctly, but there is no published package supplying
> TypeScript definitions for the host API — you will not get autocomplete on `ctx` today.
> [`docs/architecture.md`](docs/architecture.md) §6 documents the full surface in the meantime.

## Supported languages

| Language | id | Extensions |
| --- | --- | --- |
| TypeScript | `typescript` | `.ts`, `.mts`, `.cts` |
| TSX | `tsx` | `.tsx` |
| JavaScript | `javascript` | `.js`, `.mjs`, `.cjs`, `.jsx` |
| Python | `python` | `.py`, `.pyi` |
| Go | `go` | `.go` |

Each carries syntactic binding resolution, so a rule can ask where a name came from rather than
matching text: `ctx.bindingKind`, `ctx.resolvesToImport` and `ctx.isShadowed` answer for all five.
That is what stops a rule about an imported `makeStyles` from firing on a local variable of the
same name.

**The grammar is chosen by the file, not by the rule.** A rule declares which languages it applies
to and does not run on files of any other language. A rule that omits `language` defaults to
`['typescript', 'tsx']`.

## Configuration

`lanekeep.json`, at the project root. `lanekeep init` writes one for you, matched to the
project it finds.

```json
{
  "$schema": "https://raw.githubusercontent.com/fmsouza/lanekeep/main/schema/lanekeep.schema.json",

  "include": ["**/*.go"],
  "exclude": ["**/*_test.go"],

  "rules": [
    "lanekeep/no-package-init",
    { "rule": "lanekeep/no-restricted-imports", "options": { "restrictions": [
      { "module": "database/sql", "from": ["!internal/store/**"], "reason": "go through the store package" }
    ] } },
    "./lanekeep/rules/no-fmt-println.ts"
  ]
}
```

A string uses a rule as it comes; the object form calls it with options. `$schema` is what
gives you **completion and validation in your editor with nothing installed** — VS Code and
most others read it directly.

**Rules are TypeScript, configuration is not.** A rule is a program, and that is the point of
the tool; saying which rules to run is data. A Go or Python team should not have to write a
`.ts` file to do the second, which is why the config is JSON and only the rules are not.

Rule ids are namespaced. `lanekeep/` is reserved for built-ins and `local/` needs no
declaration; any other prefix must be listed in `namespaces`, so a typo in an id is an error
rather than a rule that silently never runs.

Eight rules ship built in — four for TypeScript and JavaScript, two for Python, two for Go. See
[`docs/built-in-rules.md`](docs/built-in-rules.md) for what each one checks and its options.

<details>
<summary>Configuring in TypeScript instead</summary>

`lanekeep.config.ts` still works, and is the better choice when the config computes something
or shares a preset across repositories — composition is then ordinary `import`, with no
bespoke `extends` mechanism to learn.

```ts
import { defineConfig } from 'lanekeep'
import noDefaultExport from 'lanekeep/no-default-export'
import noDebugger from './lanekeep/rules/no-debugger'

export default defineConfig({
  include: ['src/**/*.{ts,tsx}'],
  rules: [noDefaultExport, noDebugger],
})
```

Both formats compile to the same thing before anything reads them, so they cannot differ in
behavior. `lanekeep.json` wins if a project somehow has both.

</details>

## Using it

```bash
lanekeep check                  # the whole project
lanekeep check --staged         # only what is about to be committed
lanekeep check --since main     # only what changed against a ref
lanekeep check --watch          # re-check on every change, until Ctrl-C
lanekeep check --fix            # apply the safe fixes, report what is left
lanekeep check --profile        # where the run spent its time, per rule
lanekeep rules                  # what this project has configured
lanekeep explain <rule-id>      # one rule's card, without opening its source
```

`--staged` and `--since` are intersected with the config's `include`/`exclude`, and both **skip
cross-file rules** — a whole-corpus rule over a subset gives a wrong answer rather than a smaller
one, so they are skipped and named on stderr instead of quietly producing one.

**Fixes.** Only a fix its rule marked as behavior-preserving is applied. Anything else is a
suggestion — shown, never written — because the cautious mistake costs a manual edit and the other
one rewrites your code silently.

**Suppressions** carry a mandatory reason and an optional expiry. A directive that does not work
says so, rather than silently doing nothing:

```ts
// lanekeep-ignore-next-line lanekeep/no-default-export reason: legacy entry point
export default parse
```

Run `lanekeep check --report-unused-suppressions` to find the ones that no longer silence
anything.

**Output.** `--format` takes `human` (default), `json` (versioned, stable schema), `sarif` (GitHub
code scanning) and `agent` — token-minimal, grouped by rule rather than by file, with each card
stated once instead of once per violation. Diagnostics always go to stderr, so piping into a
parser works even when something fails.

**Exit codes:** `0` clean, `1` violations found, `2` the checker could not run. A caller has to be
able to tell "your code has problems" from "the tool is broken". `--warn-only` reports violations
but exits `0`, for a phased rollout.

## In CI

```yaml
- name: lanekeep
  run: npx lanekeep check --format sarif > lanekeep.sarif
- uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: lanekeep.sarif
```

As a pre-commit hook, `--staged` is the one you want:

```bash
lanekeep check --staged
```

## In your editor, and for agents

```bash
lanekeep server
```

One binary speaking two protocols over stdio, both JSON-RPC 2.0:

- **LSP** — diagnostics as you edit, for any editor with a language client.
- **MCP** — three tools (`lanekeep_check`, `lanekeep_rules`, `lanekeep_explain`) for agent hosts,
  so an agent can ask what it broke and what the rule wants without shelling out.

Nothing is printed to stdout that is not a protocol message.

## How it stays fast with programmable rules

The usual problem with a native tool that runs JavaScript plugins is the boundary between them:
dispatching into JS once per AST node means tens of thousands of crossings per file.

lanekeep dispatches once per **query match** instead. The tree-sitter query runs in Rust across a
single shared parse; only matches reach your handler. That is typically two to three orders of
magnitude fewer crossings, and it is the reason a Rust engine still earns its place once rules are
TypeScript.

```
discover paths (globs, gitignore-aware)
  └─> for each file, in parallel:
        cache key ──hit──> validate tracked deps ──> cached violations + facts
                  └─miss─> path and raw-text gates reject before any parse
                           └─> parse ─> match queries in Rust
                               └─> invoke the TypeScript handler, per match only
  └─> reduce phase: cross-file rules consume facts only, never parse trees
  └─> filter suppressions ─> sort ─> report
```

A warm run with no changes executes no JavaScript at all — every file is a cache hit.

Violations are always sorted by `(ruleId, file, line, column)`, and the sandbox withholds the clock
and randomness, so two runs over identical input produce byte-identical output. An agent reading
the output twice must not see reordering as change.

## Installing without a package manager

Prebuilt for macOS on Apple silicon, Linux on x86-64 and arm64, and Windows on x86-64. The Linux
binaries are built against **glibc 2.17**, so they run on anything from RHEL 7 onwards.

Intel macOS is not prebuilt — `cargo install lanekeep-cli` builds it from source, and both the npm
launcher and the Homebrew formula say so rather than failing obscurely.

**No runtime is required to run lanekeep**, even though rules are written in TypeScript. Node,
Python or Go is needed only to install it from that ecosystem, where it picks which binary to
fetch. Nothing is pulled in as a dependency any of those ways.

The Go package is a small launcher, because Go can only install and pin things written in Go: it
fetches the real binary on first use, verifies it against the release's published checksums, and
caches it. Set `LANEKEEP_BINARY` to an already-installed lanekeep and it fetches nothing.

## Documentation

| Document | Purpose |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | The full design: execution model, host API, cache, milestones |
| [`docs/built-in-rules.md`](docs/built-in-rules.md) | The rules lanekeep ships with, and their options |
| [`docs/cross-file-rules.md`](docs/cross-file-rules.md) | Writing a rule that needs a whole-corpus view |
| [`docs/adr/`](docs/adr/) | Decision records: why the design is the way it is |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Setup, commands, and the pull request process |
| [`AGENTS.md`](AGENTS.md) | How to work in this repository — for coding agents and humans alike |
| [`SECURITY.md`](SECURITY.md) | Threat model and how to report a vulnerability |
| [`docs/releasing.md`](docs/releasing.md) | How a release is built, gated and published |
| [`CHANGELOG.md`](CHANGELOG.md) | What changed, per release |

## Security

lanekeep is meant to run as a pre-commit hook and inside CI, which makes it a supply-chain target.
Rules are executable code, so the posture is about confinement rather than absence:

- **No ambient authority.** Rules run in an embedded QuickJS sandbox and reach exactly the host
  functions lanekeep exposes. `fs`, `process`, `child_process`, network and dynamic import are not
  restricted — they do not exist in the context.
- **No network access.** Ever, in any mode, with no configuration that enables it.
- **Filesystem confinement.** Reads go through a tracked `ctx.readFile`, confined to the project
  root. Writes happen only under `--fix`, only to matched files, only within reported ranges.
- **Bounded execution.** A per-invocation timeout, a global run budget and a per-runtime memory
  ceiling, none disableable — a rule that hangs a pre-commit hook is indistinguishable from a
  broken tool. Breaching any of them cancels the run and exits `2`, rather than reporting a partial
  result as a clean one.
- **Deterministic by construction.** The sandbox withholds the clock and randomness, so a rule
  cannot introduce nondeterminism even by accident.

This bounds blast radius and makes third-party rule sets reviewable. It is not a boundary against
someone who can already commit to the repository being checked. To report a vulnerability, see
[`SECURITY.md`](SECURITY.md).

## Project status

**Released and usable.** The current version is on [crates.io](https://crates.io/crates/lanekeep-cli),
[npm](https://www.npmjs.com/package/lanekeep), [PyPI](https://pypi.org/project/lanekeep/), Homebrew,
and as a Go module — one build feeding every channel, so the bytes are identical whichever you use.

It is **0.x**, and this repository treats that as semver does: a minor bump may break a public Rust
API. Rule authors are insulated from that — `ctx` methods and the config shape are additive — but
pin a version if you embed the crates.

Known gaps, stated rather than implied:

- **No editor types for rule authors yet** (above).
- **The performance budgets in [`docs/architecture.md`](docs/architecture.md) §15 are not met.**
  They are targets, and that document says by how much and what the levers are. The tool is fast;
  the numbers are simply ambitious.
- **No type-aware analysis**, by design. Binding resolution is syntactic — see §1 non-goals.

## Contributing

Contributions are welcome, particularly new built-in rules and new host API surface. Start with
[`CONTRIBUTING.md`](CONTRIBUTING.md) — `./scripts/setup-dev.sh` installs everything and wires the
git hooks, and `just check` is the same gate CI runs.

All work ships as squashed pull requests with
[Conventional Commits](https://www.conventionalcommits.org/) titles. `main` is protected and takes
no direct pushes.

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([`LICENSE-MIT`](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
