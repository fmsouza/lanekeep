# lanekeep — Architecture

Deterministic, AST-based architectural conformance checking for AI-generated and human-written code.

Not a linter in the ESLint sense. ESLint enforces language-level correctness; lanekeep enforces project-specific conventions an LLM has no way to infer from the code it is shown. Every rule is a codified answer to *"the agent keeps doing this wrong."*

---

## 1. Scope

### v1 goals

- TypeScript / JavaScript (incl. TSX/JSX).
- Declarative rules only: authored in config, evaluated in Rust.
- Built-in rules contributed upstream by PR.
- One-shot CLI as the default execution model.
- Content-addressed cache; incremental by file content.
- Fast enough for the inner loop: agents and developers invoke it after every edit.

### Explicit non-goals for v1

- **No plugin system.** No WASM host, no napi/PyO3 rule hosts, no third-party code execution. See §11 for why, and what is preserved so this stays additive.
- No type-aware analysis. Light binding resolution only (§6.C).
- No autofix in M0 (arrives M2, §12).
- No LSP/MCP server in M0 (arrives M3).

### Distribution

Single static Rust binary. Shipped via npm (platform packages + thin wrapper, the esbuild/swc/Biome pattern), cargo, and Homebrew. Without an in-process plugin host there is no reason to ship a napi addon — the wrapper just execs the right binary.

---

## 2. Execution model

```
discover paths (globs, gitignore-aware)
  └─> for each file, in parallel (rayon):
        cache key (§5) ──hit──> cached violations + facts
                       └─miss─> read bytes
                                └─> cheap pre-parse reject (C0/C1 predicates, §6)
                                    └─> tree-sitter parse
                                        └─> run compiled queries (one pass)
                                            └─> evaluate predicates on captures
                                                └─> emit violations + facts
                                                └─> write cache entry
  └─> reduce phase: cross-file rules consume facts only (never trees)
  └─> filter suppressions
  └─> sort (ruleId, file, line, column)
  └─> report
```

Two invariants worth defending:

1. **The reduce phase never touches parse trees.** Facts are small, serializable, and cacheable, so cross-file rules stay parallel and incremental. This is the fix for the `finalize` bottleneck in the original design, where cross-file checks forced whole-corpus in-process execution.
2. **Everything is pure given `(bytes, path, ruleset, config)`.** No I/O inside predicates. This is what makes the cache sound.

### Concurrency

`par_iter()` over files with rayon. No worker pool, no path sharding, no result serialization, no per-worker parser init. The entire `worker.ts` / `dispatch` layer from the original stops existing.

Shared-memory means the round-robin imbalance problem also disappears: rayon's work-stealing handles the one 4k-line file that would otherwise stall a shard.

---

## 3. Crate layout

```
lanekeep/
  crates/
    lanekeep-core/       # walker, query evaluation, predicate engine, facts, violations
    lanekeep-query/      # query parsing + compilation
    lanekeep-lang/       # Language trait + registry
    lanekeep-lang-js/    # tree-sitter grammars + JS binding resolution
    lanekeep-config/     # schema, extends resolution, canonicalization + hashing
    lanekeep-cache/      # content-addressed store
    lanekeep-rules/      # built-in rules
    lanekeep-report/     # human, json, sarif, agent reporters
    lanekeep-cli/        # binary
    lanekeep-server/     # LSP + MCP           (M3 — not created in v1 scope)
    lanekeep-testkit/    # RuleTester: fixture-based snapshot harness
  npm/                   # platform packages + wrapper
  benches/
  docs/
```

`lanekeep-testkit` is not optional. Without a `RuleTester` equivalent, community rule contributions are unreviewable.

### The Language trait

```rust
pub trait Language {
    fn id(&self) -> &'static str;
    fn extensions(&self) -> &[&'static str];
    fn grammar(&self) -> tree_sitter::Language;
    fn grammar_abi(&self) -> u16;

    /// Language-specific light semantics. JS resolves imports and
    /// local bindings; other languages may return Unsupported.
    fn resolver(&self) -> &dyn BindingResolver;
}
```

Implement this on day one even though only `lanekeep-lang-js` exists. It is cheap now and impossible to retrofit.

---

## 4. Rule definition format

A rule is: a tree-sitter query producing named captures, plus predicates attached to those captures, plus the human/agent-facing payload.

```yaml
- id: local/no-numeric-sizes
  language: typescript
  severity: error
  message: "Literal numeric size inside makeStyles"
  remediation: "Use theme.spacing.*, theme.borderRadius.* or theme.borders.*"
  examples:
    bad:  "padding: 12"
    good: "padding: theme.spacing.md"

  query: |
    (pair
      key: (property_identifier) @prop
      value: [(number) (unary_expression operand: (number))] @value) @match

  where:
    all:
      - prop:  { name-matches: "^(padding|margin|gap|width|height|borderRadius)" }
      - value: { numeric-value: { ne: 0 } }
      - match:
          inside:
            query: "(call_expression function: (identifier) @f)"
            where:
              f: { resolves-to-import: { module: "@rneui/themed", name: "makeStyles" } }

  report-at: match
```

**Predicates attach to captures.** That is the design decision that keeps this a vocabulary rather than a language: composition happens through the query's capture structure, not through arbitrary predicate nesting.

`message`, `remediation` and `examples` are mandatory. They are not documentation — they are the **rule card**, consumed by `lanekeep explain`, by the agent reporter, and by context injection so the agent learns the rule *before* generating rather than after.

---

## 5. Cache

```rust
key = blake3(
    engine_version_major_minor,   // bump breaks cache intentionally
    query_lang_version,
    grammar_id, grammar_abi,      // per language
    ruleset_hash,                 // canonicalized rule definitions
    config_hash,                  // severity, include/exclude, options
    file_relative_path,           // path predicates exist — path is an input
    file_content_hash,            // blake3 of bytes
)
```

Value: `{ violations, facts, suppressions }`.

Suppressions belong in the cache entry because directives are parsed during the map phase, and a
reduce-phase violation may be reported at a definition site in a file that was not reprocessed
this run. A cache entry holding only violations and facts would lose the directive and report a
suppressed violation on the warm path.

Three things people get wrong here, all of which are silent-staleness bugs:

- **`ruleset_hash` must be over canonicalized rule definitions**, not the config file's bytes. Reformatting the YAML must not invalidate; editing a regex must.
- **Relative path belongs in the key.** Path predicates make results path-sensitive; a moved file with identical bytes is not a cache hit.
- **Grammar ABI belongs in the key.** A tree-sitter grammar bump changes node shapes and therefore query results.

Storage: single memory-mapped file under `.lanekeep/cache`, not per-file entries (inode churn dominates at 2k+ files). Cache is disposable — corrupt or unreadable means full recompute, never an error.

### Incremental entry points

- `--since <ref>` — files changed vs a git ref.
- `--staged` — files in the index. Pre-commit hook default.
- Neither flag — full discovery, but warm cache makes it cheap.

---

## 6. Predicate vocabulary

Designed as a set, not accreted one rule at a time. Five cost classes; the engine **sorts predicates by cost and short-circuits**, so a cheap path predicate rejects before any file is read.

### C0 — Path (no file read)

| Predicate | Notes |
|---|---|
| `path-matches: <glob>` | |
| `path-not-matches: <glob>` | Rule-scoped exemptions, e.g. the wrapper components directory |

### C1 — Raw text (read, no parse)

| Predicate | Notes |
|---|---|
| `file-contains: <literal>` | Substring scan (memchr). Pure pre-parse reject. |
| `file-not-contains: <literal>` | |

The single largest perf lever available. A rule scoped to `makeStyles` can skip parsing every file whose bytes don't contain that string.

### C2 — Node-local (parsed, O(1))

| Predicate | Notes |
|---|---|
| `kind: <node-kind>` | |
| `text-matches: <regex>` / `text-equals: <string>` | |
| `name-matches: <regex>` | Identifier names |
| `is-literal: {numeric\|string\|boolean\|null}` | |
| `numeric-value: {eq\|ne\|lt\|lte\|gt\|gte\|in}` | Covers "0 is allowed" |
| `child-count: {eq\|lt\|gt}` | |

### C3 — Structural (O(depth) or O(subtree))

| Predicate | Notes |
|---|---|
| `inside: {query, where?}` | Any ancestor matches |
| `not-inside: {query, where?}` | |
| `direct-parent: {query, where?}` | |
| `contains: {query, where?}` | Any descendant matches |
| `preceded-by` / `followed-by` | Siblings |
| `inside-handler-of: {try-catch\|promise-catch\|both}` | Unifies the two syntactic forms of "in an error handler" |
| `inside-function-kind: {async\|generator\|arrow\|method}` | |

### C4 — Binding (O(file), memoized per file)

| Predicate | Notes |
|---|---|
| `resolves-to-import: {module, name}` | Handles aliasing: `import { makeStyles as ms }` |
| `is-imported-from: <module glob>` | |
| `binding-kind: {const\|let\|var\|param\|function\|class\|import}` | |
| `is-shadowed` | Locally rebound identifier |

C4 is the light semantic layer that pure syntactic matching gets wrong, and it is exactly what `resolveNamedImport` / `getMakeStylesBinding` were doing by hand in the original.

### Combinators

`all`, `any`, `not`. That is the complete list.

### The line that stops this becoming a language

A predicate may not: iterate, accumulate state, perform arithmetic beyond comparison, call other rules, or read anything outside the current file plus the resolved ruleset. Anything requiring those becomes a **built-in Rust rule**, not a config extension.

Hold this line deliberately. The expressiveness ceiling is the price of having no escape hatch, and the right response to hitting it is a new built-in predicate or a built-in rule — never a new construct in the config language.

---

## 7. Configuration

```yaml
# lanekeep.yaml
extends:
  - ./presets/react-native.yaml     # v1: local paths only

include:
  - "apps/*/src/**/*.{ts,tsx}"
  - "packages/*/src/**/*.{ts,tsx}"
exclude:
  - "**/*.{test,spec}.{ts,tsx}"
  - "**/__tests__/**"

severity:
  lanekeep/no-default-export: warn
  local/no-numeric-sizes: error

rules:
  - id: local/no-numeric-sizes
    # ...
```

`extends:` ships in v1 resolving **local paths only**. Presets as files inside the repo. Costs almost nothing, gives teams composition immediately, and the eventual package-resolution version is a resolver swap behind identical syntax.

Merge order is deterministic: later `extends` entries override earlier; the consuming file overrides all; `severity` is applied last.

---

## 8. Suppressions

```ts
// lanekeep-ignore-next-line local/no-numeric-sizes reason: legacy API requires exact 44
minWidth: 44,

// lanekeep-ignore-file local/no-primitive-components reason: generated fixture
```

- Rule IDs whitespace- or comma-separated. `reason:` mandatory.
- Directive must be a standalone token, so prose mentioning it doesn't match.
- `--report-unused-suppressions` — hygiene. A suppression whose violation no longer exists is debt.
- Optional `expires: YYYY-MM-DD` — surfaces as a violation past the date. Makes "temporary" suppressions actually temporary.

---

## 9. Output

| Format | Purpose |
|---|---|
| `human` | Default. Path:line:col, rule id, message, indented remediation. Colors auto-off when not a TTY or `NO_COLOR`. |
| `json` | Machine-readable, stable schema, versioned. |
| `sarif` | Free GitHub code-scanning integration. |
| `agent` | Token-minimal, remediation-first, grouped by rule rather than by file. Includes rule cards for violated rules only. |

Violations sorted `(ruleId, file, line, column)` always. Deterministic output matters more than usual here: an agent reads it twice and must not see reordering as change.

**Exit codes:** `0` clean or `--warn-only`; `1` violations; `2` runtime error.

---

## 10. CLI surface

```
lanekeep init
lanekeep check [paths...]
        [--since <ref> | --staged]
        [--format human|json|sarif|agent]
        [--warn-only] [--profile] [--no-cache]
lanekeep check --watch          # foreground, incremental, re-runs on change
lanekeep explain <rule-id>      # prints the rule card
lanekeep rules [--json]
lanekeep server                 # M3 — LSP + MCP, launched by editors/agent hosts
```

One-shot is the default and CI runs it unchanged. `--watch` is a foreground loop, not a background daemon. `server` is explicit and separate. The warm cache is what makes one-shot fast; process persistence is an optimization for editors, not a requirement for speed.

---

## 11. Security posture

With no plugin system, the posture is simple enough to state in full:

- **No code execution.** Rules are data. No `eval`, no dynamic loading, no sandbox needed because there is nothing to sandbox.
- **No network.** Ever, in any mode.
- **Reads** only files matching resolved `include` globs. **Writes** only with `--fix`, only to matched files, only within reported ranges.
- Every rule in existence is reviewed by a maintainer — a considerably stronger position than any sandbox would give.
- Supply chain: npm provenance / sigstore attestations, CI pinned to SHAs, `cargo-deny` + `cargo-audit` in CI, minimal dependency surface. This is a pre-commit-hook-adjacent tool and therefore a target.

---

## 12. One-way doors

Cheap now, breaking changes later. Lock all four before writing much code.

1. **Namespaced rule IDs from day one.** `lanekeep/<id>` for built-ins, `local/<id>` for config-authored. Bare IDs in v1 would break every config file, every suppression comment, and every consumer parsing JSON output when namespaces arrive. This is the expensive one.
2. **`ruleset_hash` already in the cache key** (§5), so adding future sources extends the input rather than changing the schema.
3. **`extends:` in v1**, local-path resolution only.
4. **A clean internal `Rule` boundary**, even though only built-ins implement it. If built-in rules reach into arena handles, cache state, or walker internals, the boundary can never be exposed. Treat it as public API you happen not to have published.

Together these mean a plugin system, should it ever arrive, is purely additive: a new `Rule` implementor, a new hash input, a new resolver behind existing config syntax.

---

## 13. Performance budget

Targets, gated in CI with regression thresholds:

| Scenario | Budget |
|---|---|
| Cold full run, ~2k files, ~20 rules | < 500 ms |
| Warm run, no changes | < 25 ms |
| Warm run, 1 changed file | < 5 ms |

Reference point: the original TypeScript implementation ran ~1.3 s cold on ~1,800 files, of which parsing was roughly 87% of CPU. Removing the TS compiler API and the per-run reparse is where nearly all of the improvement comes from — the walk itself was never the bottleneck.

Instrumentation behind `--profile` only. The original allocated a closure and took two timer readings per node per handler in the hot loop; that cost is real and must not be paid by default.

`benches/` runs on a fixed corpus, in CI, with a hard failure on regression beyond threshold.

---

## 14. Milestones

**M0 — walking skeleton.** Workspace, config parsing, discovery, tree-sitter TS/TSX, query compilation, ~6 predicates (`path-matches`, `name-matches`, `numeric-value`, `inside`, `not-inside`, `resolves-to-import`), human + json reporters, `RuleTester`. Acceptance: the four built-in rules and a representative set of config-authored `local/*` rules run end-to-end against the fixture corpus, with snapshot-verified output in every reporter.

**M1 — speed.** Cache, `--since` / `--staged`, rayon parallelism, `benches/` in CI with regression gates. Acceptance: budgets in §13 met.

**M2 — completeness.** Full predicate vocabulary, cost-class ordering, SARIF + agent reporters, `explain`, fixes (template-based replacement of a capture, marked machine-applicable vs suggestion), unused-suppression reporting.

**M3 — loops.** `--watch`, then `server` (LSP + MCP).

**M4 — second language.** Python is the cheapest proof that the `Language` trait abstraction actually holds. If adding it requires touching `lanekeep-core`, the abstraction was wrong and it is far better to learn that at M4 than at M10.

---

## 15. Resolved decision — tier-1 query language

**Resolved: tree-sitter S-expression queries.** See [ADR-0003](adr/0003-tree-sitter-queries-over-gritql.md).

Tree-sitter queries are free, already multi-language, and keep the surface small — complexity gets absorbed into predicates, which are just Rust functions addable without touching the grammar. Their weakness is negation and scope handling, which is precisely what the C3/C4 predicates exist to cover.

GritQL was evaluated and declined for v1. Its principal advantage is the rewrite operator, which serves autofix — and §12's autofix design is template-based replacement of a named capture, which does not need it. Biome's own GritQL plugin remains diagnostic-only, so the most mature adopter has not yet realised the advantage. GritQL is itself built on tree-sitter, so adopting it adds a substantial layer rather than replacing a dependency.

Given there is no plugin escape hatch, this choice sets the tool's expressiveness ceiling. Adopting GritQL later stays additive: a second compiler behind the existing `query:` field, selected by a per-rule dialect marker.
