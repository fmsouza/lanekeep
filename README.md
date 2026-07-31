# lanekeep

**Deterministic, AST-based architectural conformance checking for AI-generated and human-written code.**

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

> **Status: early development.** Nothing is released yet and the CLI described below is not
> usable. The architecture is settled — see [`docs/architecture.md`](docs/architecture.md) — and
> the work is tracked as a sequence of milestones. Do not depend on this yet.

---

## What it is

lanekeep is not a linter in the ESLint sense. ESLint enforces language-level correctness.
lanekeep enforces *project-specific conventions* — the ones a language model has no way to infer
from the code it is shown, because they live in your team's heads and your reviewers' comments.

Every rule is a codified answer to **"the agent keeps doing this wrong."**

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

## Why it exists

An agent that writes code against your codebase will violate your conventions confidently and
repeatedly, because those conventions are invisible in the code it was shown. Telling it again in
the next prompt does not scale. Encoding the convention as a rule does.

That makes the design constraints unusual for a static analyzer:

- **It runs in the inner loop.** Agents and developers invoke it after every edit, so a cold run
  on a couple of thousand files has a sub-second budget and a warm run has a sub-25ms one.
- **Its output is read by a machine.** Violations are sorted deterministically, because an agent
  that reads the output twice must not see reordering as change.
- **Every rule carries its own fix.** `message`, `remediation` and `examples` are mandatory
  fields, not documentation — they are the rule card that gets fed back to the agent.

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

## Installation

Not yet published. When it ships it will be a single static binary with the JavaScript engine
compiled in, available via npm, cargo and Homebrew. **Node.js is not required to run lanekeep**,
even though rules are written in TypeScript.

## What it looks like

```
$ lanekeep check
src/also.ts:2:1 error [lanekeep/no-default-export] default export
  → use a named export, so the symbol has one name every importer must use
src/bad.ts:2:1 error [lanekeep/no-default-export] default export
  → use a named export, so the symbol has one name every importer must use

✖ 2 error(s) across 2 file(s) checked
```

Exit `0` when clean, `1` when violations are found, `2` when the checker could not run —
a caller has to be able to tell "your code has problems" from "the tool is broken".
`--format json` emits a versioned, stable schema on stdout; diagnostics always go to stderr,
so piping into a parser works even when something fails.

## Documentation

| Document | Purpose |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | The full design: execution model, host API, cache, milestones |
| [`docs/built-in-rules.md`](docs/built-in-rules.md) | The rules lanekeep ships with, and their options |
| [`AGENTS.md`](AGENTS.md) | How to work in this repository — for coding agents and humans alike |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Setup, commands, and the pull request process |
| [`SECURITY.md`](SECURITY.md) | Threat model and how to report a vulnerability |

## Security

lanekeep is meant to run as a pre-commit hook and inside CI, which makes it a supply-chain target.
Rules are executable code, so the posture is about confinement rather than absence:

- **No ambient authority.** Rules run in an embedded QuickJS sandbox and reach exactly the host
  functions lanekeep exposes. `fs`, `process`, `child_process`, network and dynamic import are not
  restricted — they do not exist in the context.
- **No network access.** Ever, in any mode, with no configuration that enables it.
- **Filesystem confinement.** Reads go through a tracked `ctx.readFile`, confined to the project
  root. Writes happen only under `--fix`, only to matched files, only within reported ranges.
- **Bounded execution.** A per-invocation timeout, a 15-second global run budget and a per-runtime
  memory ceiling, none disableable — a rule that hangs a pre-commit hook is indistinguishable from
  a broken tool. Breaching any of them cancels the run and exits `2`, rather than reporting a
  partial result as a clean one.
- **Deterministic by construction.** The sandbox withholds the clock and randomness, so a rule
  cannot introduce nondeterminism even by accident.

This bounds blast radius and makes third-party rule sets reviewable. It is not a boundary against
someone who can already commit to the repository being checked. To report a vulnerability, see
[`SECURITY.md`](SECURITY.md).

## Contributing

Contributions are welcome, particularly new built-in rules and new host API surface. Start
with [`CONTRIBUTING.md`](CONTRIBUTING.md) — `./scripts/setup-dev.sh` installs everything and
wires the git hooks.

All work ships as squashed pull requests with [Conventional Commits](https://www.conventionalcommits.org/)
titles. `main` is protected and takes no direct pushes.

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([`LICENSE-MIT`](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
