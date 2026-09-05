# lanekeep

**Deterministic, AST-based architectural conformance checking for AI-generated and human-written code.**

[![crates.io](https://img.shields.io/crates/v/lanekeep-cli?label=crates.io)](https://crates.io/crates/lanekeep-cli)
[![npm](https://img.shields.io/npm/v/lanekeep?label=npm)](https://www.npmjs.com/package/lanekeep)
[![PyPI](https://img.shields.io/pypi/v/lanekeep?label=pypi)](https://pypi.org/project/lanekeep/)
[![CI](https://github.com/fmsouza/lanekeep/actions/workflows/ci.yml/badge.svg)](https://github.com/fmsouza/lanekeep/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![MSRV](https://img.shields.io/badge/MSRV-1.94-blue.svg)](Cargo.toml)

lanekeep enforces the conventions that live in your team's heads and your reviewers' comments —
the ones a language model cannot infer from the code it is shown. Every rule is a codified answer
to **"the agent keeps doing this wrong."**

It ships as a single static binary with no runtime dependency.

---

## Languages

Each guide covers installing lanekeep in that ecosystem, configuring it, the built-in rules that
apply, a worked custom rule, and the name-resolution behavior specific to that language.

| Language | Extensions | Install | Guide |
| --- | --- | --- | --- |
| Go | `.go` | `go get -tool github.com/fmsouza/lanekeep/cmd/lanekeep` | **[Go](https://github.com/fmsouza/lanekeep/wiki/Go)** |
| Python | `.py`, `.pyi` | `pip install lanekeep` | **[Python](https://github.com/fmsouza/lanekeep/wiki/Python)** |
| Rust | `.rs` | `cargo install lanekeep-cli` | **[Rust](https://github.com/fmsouza/lanekeep/wiki/Rust)** |
| TypeScript / JavaScript | `.ts`, `.mts`, `.cts`, `.tsx`, `.js`, `.mjs`, `.cjs`, `.jsx` | `npm install --save-dev lanekeep` | **[TypeScript and JavaScript](https://github.com/fmsouza/lanekeep/wiki/TypeScript-and-JavaScript)** |

`brew install fmsouza/tap/lanekeep` works anywhere, as does a binary from the
[releases page](https://github.com/fmsouza/lanekeep/releases). Every channel delivers the same
build, so the bytes are identical whichever you pick.

Whatever the project, the first two commands are the same:

```bash
lanekeep init     # detects the project and writes a config plus a starter rule
lanekeep check
```

New here? **[Getting Started](https://github.com/fmsouza/lanekeep/wiki/Getting-Started)** is about
a minute, end to end.

## What it is

lanekeep is not a linter in the ESLint sense. ESLint enforces language-level correctness; lanekeep
enforces *project-specific* conventions. The two barely overlap, and lanekeep replaces neither your
linter nor your formatter.

A rule is a **program**, not a configuration entry. It declares a
[tree-sitter query](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html)
that Rust matches at native speed, and a handler that runs only on matches — where it can loop,
accumulate state, read other files and ask where a name came from.

That matters because the conventions worth enforcing are the ones specific enough that nobody
else would ever write them, which is exactly the population a fixed vocabulary of predicates
fails. See **[Writing Rules](https://github.com/fmsouza/lanekeep/wiki/Writing-Rules)** for the
anatomy and the full host API, and each language guide for a worked example in that language.

Three things follow from who reads the output:

- **Every rule carries its own fix.** `message`, `remediation` and `examples` are mandatory
  fields, not documentation — they are the card fed back to whoever has to act on the violation,
  increasingly an agent.
- **Output is deterministic.** Violations are always sorted by `(ruleId, file, line, column)`, and
  the sandbox withholds the clock and randomness, so two runs over identical input produce
  byte-identical output. An agent reading it twice must not see reordering as change.
- **It runs in the inner loop.** Agents and developers invoke it after every edit, so a warm run
  is measured in tens of milliseconds. The built-ins that ship as WebAssembly components are all
  under 115 KB, so loading them is noise — a 12.4 MiB compiled-TypeScript component that once
  cost new TypeScript projects ~6.5 seconds on their first run was reverted for exactly that
  reason.
  [`docs/architecture.md`](docs/architecture.md) §15 has the ledger.

**Rules are authored in TypeScript whatever language they check** — that is the form to start
from, and it is the one most teams already have someone who writes. A rule may also be a
WebAssembly component, which is how four of the sixteen built-ins ship — two written in Rust
and two written in Go; the other twelve run as QuickJS modules, three of them checking five of
the six supported languages from a single source (every one but JavaScript). Every form reaches the same host API and is held to
the same limits, and a config names a rule rather than its implementation. **Configuration is neither** — `lanekeep.json` is
plain data, so a Go, Python or Rust team never writes a `.ts` file except when authoring an
actual rule.

## Using it

```bash
lanekeep check                  # the whole project
lanekeep check --staged         # only what is about to be committed
lanekeep check --since main     # only what changed against a ref
lanekeep check --watch          # re-check on every change, until Ctrl-C
lanekeep check --fix            # apply the safe fixes, report what is left
lanekeep check --profile        # per rule: where the time went, and what it looked at
lanekeep rules                  # what this project has configured
lanekeep explain <rule-id>      # one rule's card, without opening its source
lanekeep server                 # LSP for an editor, or --protocol mcp for an agent host
```

**Exit codes:** `0` clean, `1` violations found, `2` the checker could not run. A caller has to be
able to tell "your code has problems" from "the tool is broken". `--warn-only` reports violations
but exits `0`, for a phased rollout.

**Output formats** via `--format`: `human` (default), `json` (versioned, stable schema), `sarif`
(GitHub code scanning), and `agent` — token-minimal, grouped by rule rather than by file. Diagnostics
always go to stderr, so piping into a parser works even when something fails.

**Fixes** are applied only when the rule marked them behavior-preserving; anything else is shown
and never written. **Suppressions** carry a mandatory reason and an optional expiry, and a directive
that does not work says so rather than silently doing nothing.

Configuration reference, CI recipes, editor setup and the MCP tool list are in the
[wiki](https://github.com/fmsouza/lanekeep/wiki).

## How it stays fast with programmable rules

The usual problem with a native tool that runs JavaScript plugins is the boundary between them:
dispatching into JS once per AST node means tens of thousands of crossings per file.

lanekeep dispatches once per **query match** instead. The query runs in Rust across a single shared
parse; only matches reach your handler. That is typically two to three orders of magnitude fewer
crossings, and it is the reason a Rust engine still earns its place once rules are programs.

```
discover paths (globs, gitignore-aware)
  └─> for each file, in parallel:
        cache key ──hit──> validate tracked deps ──> cached violations + facts
                  └─miss─> path and raw-text gates reject before any parse
                           └─> parse once ─> match queries in Rust
                               └─> invoke the handler, per match only
  └─> reduce phase: cross-file rules consume facts only, never parse trees
  └─> filter suppressions ─> sort ─> report
```

A warm run with no changes executes no JavaScript at all — every file is a cache hit.

## Platforms

Prebuilt for macOS on Apple silicon, Linux on x86-64 and arm64, and Windows on x86-64. The Linux
binaries are built against **glibc 2.17**, so they run on anything from RHEL 7 onwards.

**No runtime is required to run lanekeep.** Node, Python or Go is needed only to install it from
that ecosystem, where it picks which binary to fetch. Nothing is pulled in as a dependency any of
those ways.

Intel macOS is not prebuilt — `cargo install lanekeep-cli` builds it from source, and both the npm
launcher and the Homebrew formula say so rather than failing obscurely.

## Security

lanekeep is meant to run as a pre-commit hook and inside CI, which makes it a supply-chain target.
Rules are executable code, so the posture is about confinement rather than absence:

- **No ambient authority.** A TypeScript rule runs in an embedded QuickJS sandbox and a
  WebAssembly rule under wasmtime; both reach exactly the host functions lanekeep exposes. `fs`,
  `process`, `child_process`, network and dynamic import are not restricted — they do not exist in
  the context. A component imports one interface and is refused at load if it imports another.
- **No network access.** Ever, in any mode, with no configuration that enables it.
- **Filesystem confinement.** Reads go through a tracked host call, confined to the project root.
  Writes happen only under `--fix`, only to matched files, only within reported ranges.
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

**Released and usable**, on every channel in the table above — one build feeding all of them.

It is **0.x**, and this repository treats that as semver does: a minor bump may break a public Rust
API. Rule authors are insulated from that — host API methods and the config shape are additive —
but pin a version if you embed the crates.

Known gaps, stated rather than implied:

- **Two of the three performance budgets in [`docs/architecture.md`](docs/architecture.md)
  §15 are not met.** The cold budget is; they are targets, and that section says by how much
  and where the remaining time goes.
- **No type-aware analysis**, by design. Name resolution is syntactic — see §1 non-goals.

## Documentation

**The [wiki](https://github.com/fmsouza/lanekeep/wiki) is the place to start** — it is task-shaped
and organized by language.

| Page | Purpose |
| --- | --- |
| [Getting Started](https://github.com/fmsouza/lanekeep/wiki/Getting-Started) | Install and catch something, in about a minute |
| [Configuration](https://github.com/fmsouza/lanekeep/wiki/Configuration) | `lanekeep.json`, every field |
| [Writing Rules](https://github.com/fmsouza/lanekeep/wiki/Writing-Rules) | Rule anatomy and the full host API |
| [CI and Editors](https://github.com/fmsouza/lanekeep/wiki/CI-and-Editors) | Pre-commit, GitHub Actions, LSP, MCP |

In-repo, versioned with the code:

| Document | Purpose |
| --- | --- |
| [`docs/architecture.md`](docs/architecture.md) | The full design: execution model, host API, cache, milestones |
| [`docs/built-in-rules.md`](docs/built-in-rules.md) | The rules lanekeep ships with, and their options |
| [`docs/cross-file-rules.md`](docs/cross-file-rules.md) | Writing a rule that needs a whole-corpus view |
| [`docs/obligation-rules.md`](docs/obligation-rules.md) | Writing a rule that needs a resource released on every path |
| [`docs/authoring-rust-rules.md`](docs/authoring-rust-rules.md) | Writing a rule in Rust, shipped as a WebAssembly component |
| [`docs/authoring-go-rules.md`](docs/authoring-go-rules.md) | Writing a rule in Go |
| [`docs/authoring-python-rules.md`](docs/authoring-python-rules.md) | Writing a rule in Python |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Setup, commands, and the pull request process |
| [`AGENTS.md`](AGENTS.md) | How to work in this repository — for coding agents and humans alike |
| [`SECURITY.md`](SECURITY.md) | Threat model and how to report a vulnerability |
| [`docs/releasing.md`](docs/releasing.md) | How a release is built, gated and published |
| [`CHANGELOG.md`](CHANGELOG.md) | What changed, per release |

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
