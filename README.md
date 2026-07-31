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

```yaml
# lanekeep.yaml
rules:
  - id: local/no-numeric-sizes
    language: typescript
    severity: error
    message: "Literal numeric size inside makeStyles"
    remediation: "Use theme.spacing.*, theme.borderRadius.* or theme.borders.*"

    query: |
      (pair
        key: (property_identifier) @prop
        value: [(number) (unary_expression operand: (number))] @value) @match

    where:
      all:
        - prop:  { name-matches: "^(padding|margin|gap|width|height|borderRadius)" }
        - value: { numeric-value: { ne: 0 } }
```

Rules are **data, not code**. There is no plugin system, no `eval`, and nothing to sandbox,
because there is nothing to execute. See [Security](#security).

## Why it exists

An agent that writes code against your codebase will violate your conventions confidently and
repeatedly, because those conventions are invisible in the code it was shown. Telling it again in
the next prompt does not scale. Encoding the convention as a rule does.

That makes the design constraints unusual for a static analyser:

- **It runs in the inner loop.** Agents and developers invoke it after every edit, so a cold run
  on a couple of thousand files has a sub-second budget and a warm run has a sub-25ms one.
- **Its output is read by a machine.** Violations are sorted deterministically, because an agent
  that reads the output twice must not see reordering as change.
- **Every rule carries its own fix.** `message`, `remediation` and `examples` are mandatory
  fields, not documentation — they are the rule card that gets fed back to the agent.

## Design in one diagram

```
discover paths (globs, gitignore-aware)
  └─> for each file, in parallel:
        cache key ──hit──> cached violations + facts
                  └─miss─> cheap pre-parse reject (path and raw-text predicates)
                           └─> parse ─> run compiled queries ─> evaluate predicates
                               └─> emit violations + facts, write cache entry
  └─> reduce phase: cross-file rules consume facts only, never parse trees
  └─> filter suppressions ─> sort ─> report
```

Two invariants hold this up. The reduce phase never touches parse trees, which is what keeps
cross-file rules parallel and incrementally cacheable. And everything is pure given
`(bytes, path, ruleset, config)`, which is what makes the cache sound.

## Installation

Not yet published. When it ships it will be a single static binary, available via npm, cargo and
Homebrew.

## Documentation

| Document | Purpose |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | The full design: execution model, predicate vocabulary, cache, milestones |
| [`docs/adr/`](docs/adr/) | Architecture decision records — what was decided, and why |
| [`docs/superpowers/specs/`](docs/superpowers/specs/) | Implementation spec: governance, testing, CI/CD, delivery sequence |

`AGENTS.md` and `CONTRIBUTING.md` arrive with the development environment.

## Security

lanekeep is meant to run as a pre-commit hook and inside CI, which makes it a supply-chain
target. The posture is deliberately narrow enough to state in full:

- **No code execution.** Rules are data. Nothing is loaded, compiled or evaluated at runtime.
- **No network access.** Ever, in any mode.
- **Reads** only files matching resolved `include` globs. **Writes** only under `--fix`, only to
  matched files, and only within reported ranges.
- Every built-in rule is reviewed by a maintainer.

To report a vulnerability, see [`SECURITY.md`](SECURITY.md).

## Contributing

Contributions are welcome, particularly new built-in rules and new predicates. Until the
contributor guide lands, [`docs/architecture.md`](docs/architecture.md) is the place to start —
§4 defines the rule format and §6 the predicate vocabulary.

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
