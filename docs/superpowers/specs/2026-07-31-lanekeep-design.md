# lanekeep — implementation spec

**Date:** 2026-07-31
**Status:** approved

## How to read this document

[`docs/architecture.md`](../../architecture.md) is the authority on the engine: execution model,
predicate vocabulary, cache key, configuration format, rule definition format. This document does
not restate it.

This spec records what the architecture document leaves open or does not cover:

1. Decisions taken against its open questions, with rationale.
2. Corrections to it that follow from those decisions.
3. Everything outside the engine — governance, toolchain, testing, CI/CD, release, agent tooling.
4. The delivery sequence.

Where the two disagree, this document wins and the architecture document gets amended.

---

## 1. Decisions

### 1.1 Greenfield — no compatibility target

lanekeep has no predecessor to remain compatible with. It is a new tool whose first users arrive
at first release. There is no output format to match, no directive syntax to accept for backward
compatibility, and no external rule set to reproduce.

**Consequence.** Acceptance criteria are defined against lanekeep's own fixture corpus and
snapshot suite, never against another implementation's behaviour.

### 1.2 Namespaced rule IDs from day one

Built-ins are `lanekeep/<id>`. Config-authored rules are `local/<id>`. Suppression directives are
`lanekeep-ignore-next-line` and `lanekeep-ignore-file`.

**Rationale.** §12's first one-way door. Retrofitting namespaces would break every config file,
every suppression comment, and every consumer parsing JSON output. It costs nothing now.

### 1.3 Tier-1 query language: tree-sitter S-expression queries

Resolves the open decision in §15. GritQL is not adopted.

**Rationale.** GritQL's principal advantage over raw tree-sitter queries is its rewrite operator,
which serves autofix — an M2 concern that template-based capture replacement already covers.
Biome's own GritQL plugin remains diagnostic-only. GritQL is a large dependency measured against
§11's minimal-surface posture, and it is itself built on tree-sitter, so adopting it does not
avoid tree-sitter — it adds a layer above it. The C3 and C4 predicate classes exist precisely to
cover tree-sitter queries' weakness at negation and scope.

**Reversibility.** Adopting GritQL later is additive: a second query compiler behind the existing
`query:` field, selected by a per-rule dialect marker. Nothing in this decision forecloses it.

### 1.4 Built-in rule catalogue for v0.1

Four rules. The set is deliberately small — the thesis is that meaningful rules are
project-specific and live in config as `local/*`.

| Rule | Class | Purpose |
| --- | --- | --- |
| `lanekeep/no-default-export` | per-file | Named exports only. Referenced by §7's config example, so already canon. |
| `lanekeep/no-restricted-imports` | per-file, parameterized | Forbid importing given modules from given paths. Validates that per-rule options work. |
| `lanekeep/no-unused-exports` | cross-file | An exported symbol no module imports. Exercises the facts/reduce join. |
| `lanekeep/no-circular-imports` | cross-file | Import cycles. Exercises facts under graph traversal rather than a join. |

**Rationale for two cross-file rules.** §2's reduce phase is the least-proven part of the
architecture. A join and a graph traversal stress it differently; validating only one leaves the
abstraction half-tested, and §14 is explicit that discovering a wrong abstraction late is the
expensive failure.

**Explicitly excluded.** Anything ESLint already covers. §1 draws this line and it is what keeps
lanekeep's identity distinct.

### 1.5 Scope: milestones M0 through M2

M0 (walking skeleton), M1 (speed), M2 (completeness) are in scope. M3 (`--watch`, LSP/MCP server)
and M4 (second language) are not. `lanekeep-server` is not created.

### 1.6 Licensing, visibility, distribution

Dual `MIT OR Apache-2.0`. Public repository. Published to crates.io and npm from the first
release, with Homebrew via cargo-dist's generated tap.

---

## 2. Corrections to `docs/architecture.md`

These are amended in the same pull request as this spec.

| § | Correction | Reason |
| --- | --- | --- |
| 5 | Cache value becomes `{ violations, facts, suppressions }` | Suppression directives are extracted during the map phase. A warm run that reads only violations and facts would lose them and report suppressed violations. See §4.3 below. |
| 14 | M0 acceptance criterion no longer references porting an external rule set | §1.1 |
| 15 | Open decision resolved | §1.3 |
| 3 | `lanekeep-server` marked out of scope rather than listed | §1.5 |

The React Native examples in §4 and §7 remain as **illustrative** config, showing what a realistic
`local/*` rule looks like. They are not an implementation target.

---

## 3. Repository and governance

### 3.1 Branch policy

`main` is protected by an active ruleset with no bypass actors: pull request required, linear
history required, squash the only permitted merge method, force-push and deletion blocked. Zero
required approvals, because the project has a single maintainer — the gate is CI, not review.

The bootstrap commit is the only commit that will ever reach `main` without a pull request. This
has been verified by attempting a direct push and confirming rejection.

### 3.2 Conventional Commits and semantic versioning

Squash merge composes the commit on `main` from the **pull request title**. The PR title is
therefore the semver-bearing artifact, not the branch's commits.

Enforcement is two-sided:

- CI validates every PR title against the Conventional Commits grammar.
- A local `commit-msg` hook validates branch commits, for changelog-quality history during review.

release-plz consumes the resulting history to compute version bumps, generate `CHANGELOG.md`, and
open a release pull request. `cargo-semver-checks` runs in CI to catch API breakage the commit
type failed to declare.

### 3.3 Repository settings

Squash-only merge with `PR_TITLE`/`PR_BODY` composition, automatic head-branch deletion, auto-merge
enabled, wiki and projects disabled, discussions enabled.

---

## 4. Architecture additions

### 4.1 The `Rule` trait

§12's fourth one-way door, made concrete:

```rust
pub trait Rule: Send + Sync {
    fn id(&self) -> &RuleId;
    fn card(&self) -> &RuleCard;         // message, remediation, examples
    fn language(&self) -> LanguageId;
    fn prefilter(&self) -> &Prefilter;   // C0/C1, evaluated before read or parse
    fn examine(&self, ctx: &mut FileCtx<'_>);      // map phase
    fn reduce(&self, _ctx: &mut ReduceCtx<'_>) {}  // default no-op
}
```

The property that matters: **config-authored rules are one implementor among several.**
`DeclarativeRule` is constructed from config and has no privileged access the built-ins lack, and
no built-in reaches past this trait into arena handles, cache state, or walker internals. Treat it
as public API that happens not to be published.

### 4.2 Facts and the reduce phase

Facts are typed, small, serializable, per-file data emitted during the map phase and cached with
the file entry. The reduce phase consumes facts plus the discovered file list — never parse trees,
never file contents.

For `lanekeep/no-unused-exports`:

- `ExportDef { symbol, line, column, is_reexport }`
- `ImportUse { module_specifier, symbols, has_namespace_import, has_dynamic_import }`

Reduce resolves specifiers to paths using the file list, joins uses onto definitions, and reports
definitions with no use. A namespace import or dynamic import against a module suppresses reporting
for that module — the same bail-out shape a dynamic property access requires.

For `lanekeep/no-circular-imports`, the same `ImportUse` facts are assembled into a module graph
and traversed for strongly connected components.

Both rules read facts that the other's map phase could have produced. That is intentional: it
proves facts are a shared medium rather than a per-rule side channel.

### 4.3 Suppressions are map-phase output

Suppression directives are parsed while the file is being processed and stored in the cache entry
alongside violations and facts. Reduce-phase violations are reported at a definition site in a
file that may not have been reprocessed this run, so its suppressions must be available from cache.

This is the correction to §5 recorded in §2 above.

### 4.4 Cache storage

Key exactly as §5 specifies, all eight inputs, blake3. A single memory-mapped file at
`.lanekeep/cache`: header with magic and format version, an index, then entries. Writes go to a
temporary file and are committed by atomic rename. Any read error — bad magic, version mismatch,
truncation, checksum failure — is treated as a cold cache. The cache is disposable by design, which
is what makes a purpose-built format acceptable rather than reckless.

**Serialization: `postcard` + `serde`.** `rkyv`'s zero-copy reads would serve the sub-25 ms warm
budget better, but cost a heavy dependency and meaningful `unsafe` against §11. The benchmark suite
lands in the same milestone as the cache and will answer whether the simpler choice holds. If it
does not, switching is a contained change behind the store's interface, justified by measurement.

### 4.5 Parse errors

tree-sitter always produces a tree, so "failed to parse" means the tree contains `ERROR` nodes.
This surfaces as a `lanekeep/parse-error` diagnostic, **warning by default**: a grammar that lags
a new TypeScript syntax feature must not break a consumer's CI. Configurable to `error`.

---

## 5. Toolchain and local/CI parity

A `justfile` is the single definition of every task. CI jobs and git hooks both invoke `just`
recipes, so "what the checks mean" exists in exactly one place and local cannot drift from CI.

| Recipe | Contents |
| --- | --- |
| `just setup` | Verify toolchain, install cargo tools, set `core.hooksPath` |
| `just check-fast` | `fmt --check`, clippy on changed crates, unit tests. Pre-commit budget. |
| `just check` | The full CI gate: fmt, clippy `-D warnings`, nextest, deny, machete, typos, doc |
| `just test` | `cargo nextest run --workspace` |
| `just bench` | Criterion suite against the fixture corpus |
| `just snapshot` | `cargo insta review` |

Git hooks live in `.githooks/`, committed and activated by `just setup`:

- `pre-commit` → `just check-fast`
- `commit-msg` → Conventional Commits validation
- `pre-push` → `just check`

`rust-toolchain.toml` pins the toolchain. MSRV is declared in the workspace manifest and verified
by a dedicated CI job, so it is a tested promise rather than a comment.

---

## 6. Testing strategy

Test-driven from the first commit: a failing test precedes every behavioural change.

| Layer | Tooling | Covers |
| --- | --- | --- |
| Unit | `cargo-nextest`, colocated `#[cfg(test)]` | Predicate evaluation, query compilation, config merge, cache keying |
| Rule | `lanekeep-testkit` `RuleTester` + `insta` | Every rule against `good`/`bad`/`skipped` fixtures |
| CLI | `assert_cmd`, `trycmd`, `insta` | Every reporter, every exit code, flag parsing |
| Property | `proptest` | Cache-key canonicalization; suppression directive parsing |
| Benchmark | `criterion`, CI-gated | §13 budgets |
| Mutation | `cargo-mutants`, nightly | Predicate engine |
| Fuzz | `cargo-fuzz`, nightly | Config and query parsing |

**Why property tests specifically there.** Both are places where a bug is silent rather than loud.
The cache-key invariants are: reformatting YAML must not change `ruleset_hash`; editing a regex
must; moving a file with identical bytes must miss. A violation of any of these produces stale
results with no error.

**Fixture corpus.** Synthetic TypeScript and TSX committed under `fixtures/`, designed to exercise
every predicate cost class, every built-in rule, and every reporter. It is the acceptance target
for M0.

---

## 7. CI/CD and release

Every action pinned to a commit SHA.

| Workflow | Trigger | Contents |
| --- | --- | --- |
| `ci.yml` | PR, push to main | fmt, clippy `-D warnings`, nextest, MSRV job, doc build, `cargo-deny`, `cargo-machete`, `typos`. Matrix: Linux, macOS, Windows. |
| `pr-title.yml` | PR opened/edited | Conventional Commits validation of the title |
| `bench.yml` | PR | Criterion against `main` baseline; fails past threshold |
| `security.yml` | PR, daily schedule | `cargo-audit`, `cargo-deny advisories`, OpenSSF Scorecard, dependency review |
| `nightly.yml` | Cron | cargo-dist build from `main` published as a `nightly` prerelease; `cargo-mutants`; short fuzz run |
| `release-plz.yml` | Push to main | Maintain the release PR; on merge, tag and publish to crates.io |
| `release.yml` | Tag | cargo-dist: all platform binaries, npm packages with provenance, Homebrew tap, SLSA attestations |

Required status checks are added to the branch ruleset **after** the workflows exist. Requiring a
check that cannot run would deadlock every pull request.

Also: `dependabot.yml` for cargo, actions and npm; `CODEOWNERS`; `SECURITY.md`;
`CONTRIBUTING.md`; `CODE_OF_CONDUCT.md`; issue and pull request templates.

### 7.1 Distribution

Single static binary. npm is the primary channel — platform packages plus a thin wrapper that
execs the right binary, the pattern esbuild, swc and Biome all use. With no in-process plugin
host there is no reason to ship a native addon. Also cargo and Homebrew.

---

## 8. Agent tooling

The requirement is agent-agnostic *and* high-signal. Those pull against each other only if content
is duplicated per vendor, so:

**Portable substrate — the only place content lives:**

- `AGENTS.md` — exact commands, crate map, TDD loop, PR conventions, and the invariants that must
  never be violated: §2's two, §6's line against the config language growing, §12's four one-way
  doors.
- `docs/playbooks/` — `add-a-rule.md`, `add-a-predicate.md`, `debug-a-query.md`,
  `optimize-a-hot-path.md`, `add-a-language.md`.
- `docs/adr/` — decision records for everything in §1 of this spec.

**Adapters — thin pointers, zero duplication:** `CLAUDE.md`, `.github/copilot-instructions.md`,
`.cursor/rules/`, `GEMINI.md`.

**Claude-specific conveniences:** `.claude/skills/` wrapping the playbooks; subagents `rule-author`,
`perf-auditor`, `query-debugger`; hooks limited to format-on-write and `just check-fast` on stop.

The test of this design: deleting every vendor-specific file must lose no information.

---

## 9. Delivery sequence

Each entry is one squashed pull request. Titles are the Conventional Commits messages that will
land on `main`.

| # | Title | Milestone |
| --- | --- | --- |
| 1 | `chore: bootstrap repository with dual license, readme and architecture doc` | done |
| 2 | `docs: record architecture decisions and implementation spec` | — |
| 3 | `chore: add cargo workspace and development environment` | — |
| 4 | `ci: add continuous integration and security workflows` | — |
| 5 | `docs: add agent documentation and contributor guides` | — |
| 6 | `feat(core): add violation, rule card and rule trait foundations` | M0 |
| 7 | `feat(config): add configuration schema with canonicalized hashing` | M0 |
| 8 | `feat(lang): add language registry and typescript grammars` | M0 |
| 9 | `feat(query): add tree-sitter query compilation` | M0 |
| 10 | `feat(core): add predicate engine with cost-class ordering` | M0 |
| 11 | `feat(core): add file discovery and parallel walker` | M0 |
| 12 | `feat(report): add human and json reporters` | M0 |
| 13 | `feat(testkit): add fixture-based rule tester` | M0 |
| 14 | `feat(rules): add no-default-export and no-restricted-imports` | M0 |
| 15 | `feat(core): add facts pipeline and reduce phase` | M0 |
| 16 | `feat(rules): add no-unused-exports and no-circular-imports` | M0 |
| 17 | `feat(cache): add content-addressed cache` | M1 |
| 18 | `feat(cli): add since and staged incremental entry points` | M1 |
| 19 | `perf: add benchmark suite and regression gates` | M1 |
| 20 | `feat(core): complete the predicate vocabulary` | M2 |
| 21 | `feat(report): add sarif and agent reporters` | M2 |
| 22 | `feat(cli): add explain command and rules listing` | M2 |
| 23 | `feat(core): add autofix support` | M2 |
| 24 | `feat(core): add unused suppression reporting` | M2 |
| 25 | `feat(dist): add npm distribution and release automation` | M2 |
| 26 | `chore: release v0.1.0` | M2 |

Milestone gates:

- **M0 complete** when every built-in and a representative set of `local/*` rules run end-to-end
  against the fixture corpus, with snapshot-verified output in every reporter.
- **M1 complete** when the §13 budgets are met and enforced by CI: cold full run under 500 ms,
  warm run under 25 ms, warm run with one changed file under 5 ms.
- **M2 complete** when the full predicate vocabulary, all four reporters, `explain`, autofix and
  unused-suppression reporting are shipped, and v0.1.0 is published.

---

## 10. Non-goals

Restating §1's exclusions plus what this spec adds:

- No plugin system, no WASM host, no third-party code execution.
- No type-aware analysis; light binding resolution only.
- No LSP or MCP server; no `--watch`.
- No second language.
- No compatibility layer for any external tool.
- No built-in rule that duplicates ESLint.

---

## 11. Deferred decisions

Recorded so they are chosen deliberately rather than by accident.

| Decision | Deferred until | Trigger |
| --- | --- | --- |
| `postcard` → `rkyv` for cache entries | M1 benchmarks | Warm-run budget missed |
| GritQL as a second query dialect | Post-v1 | A rule that the predicate vocabulary genuinely cannot express, where the answer is not a new built-in predicate |
| Plugin system | Post-v1 | Sustained external demand. Additive by construction — see §12 of the architecture. |
| `extends:` resolving package specifiers | Post-v1 | Preset sharing across repositories. A resolver swap behind identical syntax. |
