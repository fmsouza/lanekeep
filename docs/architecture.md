# lanekeep — Architecture

Deterministic, AST-based architectural conformance checking for AI-generated and human-written code.

Not a linter in the ESLint sense. ESLint enforces language-level correctness; lanekeep enforces project-specific conventions an LLM has no way to infer from the code it is shown. Every rule is a codified answer to *"the agent keeps doing this wrong."*

---

## 1. Scope

### v1 goals

- TypeScript / JavaScript (incl. TSX/JSX).
- **Rules are programs, authored in TypeScript** — the same language as the code they inspect. Turing-complete, with no expressiveness ceiling to hit.
- **Rules run sandboxed inside the binary.** An embedded JavaScript engine, reaching only host functions lanekeep chooses to expose.
- **The hot path stays in Rust.** A rule declares a tree-sitter query; Rust matches it at native speed and calls into TypeScript only on matches.
- Built-in rules ship with the tool and are authored against the same API user rules use.
- One-shot CLI as the default execution model.
- Content-addressed cache with dependency tracking; incremental by file content.
- Fast enough for the inner loop: agents and developers invoke it after every edit.

### Why rules are code, not configuration

An earlier draft of this design made rules declarative data — a query plus a fixed vocabulary of predicates, evaluated entirely in Rust. That bought a trivially safe security posture and a simple cache, at the cost of a hard expressiveness ceiling: a rule the vocabulary could not express had no remedy short of an upstream pull request.

That trade was wrong for this tool. The rules that matter are the ones specific enough that nobody else would ever write them, which is exactly the population a fixed vocabulary fails. And rule authors are TypeScript developers; asking them to learn a bespoke YAML predicate dialect to describe TypeScript is friction with nothing on the other side of it.

So rules are TypeScript programs. Everything below follows from holding that alongside the performance and determinism goals, rather than trading them away for it. See §6 for the boundary that makes it safe, and §7 for the shape that makes it fast.

### Explicit non-goals for v1

- No type-aware analysis. Light binding resolution only, exposed as host helpers (§6.4).
- No npm imports from rule code. Rules run in an embedded sandbox, not in Node (§5).
- No autofix in M0 (arrives M2, §14).
- No LSP/MCP server in M0 (arrives M3).

### Distribution

Single static Rust binary with the JavaScript engine compiled in — no Node.js required to run lanekeep, even though rules are written in TypeScript. Shipped via npm (platform packages + thin wrapper, the esbuild/swc/Biome pattern), cargo, and Homebrew.

---

## 2. Execution model

```
load + compile rule modules (once per run)
  └─> type-strip TS, resolve local imports, compile to bytecode
discover paths (globs, gitignore-aware)
  └─> for each file, in parallel (rayon):
        cache key (§8) ──hit──> validate tracked deps ──ok──> cached violations + facts
                       │                              └─stale─┐
                       └─miss────────────────────────────────┤
                                                             ↓
                                  read bytes
                                  └─> cheap pre-parse reject (path + raw-text gates, §7.1)
                                      └─> tree-sitter parse
                                          └─> run compiled queries (one pass)
                                              └─> for each match: invoke the TS handler
                                                  └─> emit violations + facts + read-deps
                                                  └─> write cache entry
  └─> reduce phase: cross-file rules consume facts only (never trees)
  └─> filter suppressions
  └─> sort (ruleId, file, line, column)
  └─> report
```

Three invariants worth defending:

1. **The reduce phase never touches parse trees.** Facts are small, serializable, and cacheable, so cross-file rules stay parallel and incremental.
2. **Everything is deterministic given `(bytes, path, ruleset, config, tracked reads)`.** Rules cannot observe time, randomness, the environment, or the network — those bindings do not exist in the sandbox (§6.6). This is what keeps the cache sound now that rules are arbitrary programs.
3. **JavaScript executes proportional to matches, never to nodes.** The query gate is what preserves the performance model; see §7.

### Concurrency

`par_iter()` over files with rayon. Each worker owns one JavaScript runtime and context, created once per run and reused across that worker's files. Rule bytecode is compiled once and instantiated per context.

Rayon's work-stealing handles the one 4k-line file that would otherwise stall a shard.

---

## 3. Crate layout

```
lanekeep/
  crates/
    lanekeep-core/       # walker, query evaluation, facts, violations, Rule trait
    lanekeep-js/         # embedded engine, sandbox, host API, module loader
    lanekeep-query/      # query parsing + compilation
    lanekeep-lang/       # Language trait + registry
    lanekeep-lang-js/    # tree-sitter grammars + JS binding resolution
    lanekeep-config/     # schema, config loading, canonicalization + hashing
    lanekeep-cache/      # content-addressed store with dependency tracking
    lanekeep-rules/      # built-in rules (authored in TypeScript, embedded at build time)
    lanekeep-report/     # human, json, sarif, agent reporters
    lanekeep-cli/        # binary
    lanekeep-server/     # LSP + MCP           (M3 — not created in v1 scope)
    lanekeep-testkit/    # RuleTester: fixture-based snapshot harness
  packages/
    lanekeep/            # the TypeScript authoring package: defineRule, defineConfig, types
  npm/                   # platform packages + wrapper
  benches/
  docs/
```

`lanekeep-testkit` is not optional. Without a `RuleTester` equivalent, community rule contributions are unreviewable.

`packages/lanekeep` is the npm package rule authors import from. It ships the `defineRule`/`defineConfig` helpers and — more importantly — the TypeScript type definitions for the entire host API, so rule authoring is autocompleted and type-checked in the author's own editor.

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

A rule is a TypeScript module with a default export: metadata, a tree-sitter query that gates execution, and a handler invoked once per match.

```ts
import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-numeric-sizes',
  language: 'typescript',
  severity: 'error',

  card: {
    message: 'Literal numeric size inside makeStyles',
    remediation: 'Use theme.spacing.*, theme.borderRadius.* or theme.borders.*',
    examples: {
      bad: 'padding: 12',
      good: 'padding: theme.spacing.md',
    },
  },

  // Evaluated in Rust before the file is read or parsed. Purely an optimization —
  // omitting them changes nothing but speed.
  gates: {
    pathMatches: ['**/*.{ts,tsx}'],
    fileContains: ['makeStyles'],
  },

  query: `
    (pair
      key: (property_identifier) @prop
      value: [(number) (unary_expression operand: (number))] @value) @match
  `,

  check(ctx, m) {
    if (!/^(padding|margin|gap|width|height|borderRadius)/.test(ctx.text(m.prop))) return
    if (Number(ctx.text(m.value)) === 0) return

    const call = ctx.closestAncestor(m.match, '(call_expression function: (identifier) @f)')
    if (!call) return
    if (!ctx.resolvesToImport(call.f, { module: '@rneui/themed', name: 'makeStyles' })) return

    ctx.report(m.match)
  },
})
```

`check` is ordinary TypeScript. It may loop, accumulate state, build data structures, read other files through `ctx.readFile`, and call any helper the author writes — including code imported from sibling rule modules. There is no expressiveness ceiling and no escape hatch needed, because the hatch is the whole floor.

An optional `timeout` field raises that rule's per-invocation budget above the default (§6.7). It exists for handlers that legitimately do heavy work, and needing it is a signal worth reading: usually the fix is a tighter query, not a longer clock.

`card.message`, `card.remediation` and `card.examples` are mandatory. They are not documentation — they are the **rule card**, consumed by `lanekeep explain`, by the agent reporter, and by context injection so the agent learns the rule *before* generating rather than after.

### Cross-file rules

A rule that needs a whole-corpus view emits facts during the per-file pass and consumes them in `reduce`:

```ts
export default defineRule({
  id: 'lanekeep/no-unused-exports',
  query: `(export_statement declaration: (_) @decl) @match`,

  // reduce() sees the whole corpus, so it gets a larger budget than the 1s default
  timeout: 5_000,

  check(ctx, m) {
    ctx.emitFact({
      kind: 'export',
      symbol: ctx.text(m.decl),
      at: ctx.loc(m.match),
    })
  },

  reduce(ctx) {
    const imported = new Set(
      ctx.facts('import').flatMap(f => f.symbols),
    )
    for (const e of ctx.facts('export')) {
      if (!imported.has(e.symbol)) ctx.report(e.at, { file: e.file })
    }
  },
})
```

`reduce` receives facts and the discovered file list. It never receives parse trees — that is invariant 1 from §2, and it is what keeps cross-file rules incremental.

---

## 5. The JavaScript host

### 5.1 Engine

QuickJS, embedded via `rquickjs`. Chosen over V8 and Boa for a combination of reasons: it compiles into a static binary at roughly a megabyte rather than tens, it starts in microseconds rather than milliseconds — which matters when the warm-run budget is 25 ms — and its runtime is straightforwardly one-per-thread, which is what rayon wants.

Its weakness is raw throughput: QuickJS interprets, where V8 compiles. That is affordable here precisely because of the query gate (§7) — JavaScript runs proportional to matches, and matches are a tiny fraction of nodes. If measurement at M1 shows handler execution dominating, the engine sits behind a trait and can be swapped without touching a rule.

### 5.2 TypeScript

Rule modules are TypeScript. Types are stripped in Rust with `oxc_transform` before evaluation — a syntactic transform, not a type check. Rules are *type-checked in the author's editor* against the types shipped in `packages/lanekeep`; lanekeep itself never type-checks, because doing so would mean shipping a TypeScript compiler and paying its cost on every run.

This is a deliberate division: the authoring experience is fully typed, the runtime is not.

### 5.3 Module loading

Rules may import from each other and from `lanekeep`. A custom ES module loader resolves:

- `lanekeep` → the host-provided module (`defineRule`, `defineConfig`, helper types)
- Relative specifiers → other files under the project's rule directory

Anything else fails at load time with a clear diagnostic. There is no `node_modules` resolution, no `require`, and no bare-specifier resolution to npm packages.

### 5.4 Compilation and reuse

Rule modules are compiled to QuickJS bytecode once per run, then instantiated in each worker's context. Compilation cost is paid once regardless of corpus size; per-worker instantiation is cheap.

---

## 6. The host API — and the boundary that replaces "no code execution"

The previous design's security posture was "rules are data, so there is nothing to execute." That is gone. What replaces it is narrower but still strong: **rules are code, but the only things they can reach are the functions lanekeep hands them.** There is no ambient `fs`, no `process`, no network, no dynamic import. Those globals are not restricted — they are absent.

This is a stronger position than a conventional plugin system offers, where a plugin inherits the full authority of the host process.

### 6.1 Reporting

| Function | Notes |
|---|---|
| `ctx.report(node \| loc, overrides?)` | Emit a violation. `overrides` may adjust message, severity, or supply a fix. |
| `ctx.loc(node)` | `{ file, line, column }` |

### 6.2 Tree navigation

| Function | Notes |
|---|---|
| `ctx.text(node)` | Source text of a node |
| `ctx.kind(node)` | Node kind |
| `ctx.parent(node)` / `ctx.children(node)` | |
| `ctx.ancestors(node)` | Lazy, outermost-last |
| `ctx.closestAncestor(node, query)` | Nearest ancestor matching a query; returns its captures |
| `ctx.querySubtree(node, query)` | Run a query scoped to a subtree |

Nodes cross the boundary as opaque integer handles into the Rust-side tree, never as materialized JavaScript objects. Materializing an AST is the cost that makes native tooling with JS plugins slow; the query gate plus handle-passing avoids paying it.

### 6.3 File and path

| Function | Notes |
|---|---|
| `ctx.filePath` | Path relative to project root |
| `ctx.fileText` | Full source text |
| `ctx.readFile(path)` | **Tracked.** Records the read as a cache dependency (§8.2). Confined to the project root; traversal outside it is an error. |
| `ctx.fileExists(path)` | Tracked identically |

### 6.4 Binding resolution

The light semantic layer that pure syntactic matching gets wrong. Implemented in Rust, memoized per file.

| Function | Notes |
|---|---|
| `ctx.resolvesToImport(node, { module, name })` | Handles aliasing: `import { makeStyles as ms }` |
| `ctx.isImportedFrom(node, moduleGlob)` | |
| `ctx.bindingKind(node)` | `const \| let \| var \| param \| function \| class \| import` |
| `ctx.isShadowed(node)` | Locally rebound identifier |

### 6.5 Facts

| Function | Notes |
|---|---|
| `ctx.emitFact(fact)` | Per-file phase. Must be JSON-serializable — facts are cached. |
| `ctx.facts(kind?)` | Reduce phase only. Aggregated across the corpus. |
| `ctx.files` | Reduce phase only. The discovered file list. |

### 6.6 What is deliberately absent

Absence is achieved two ways, and the first is much stronger.

**Never installed.** The engine's optional intrinsics are opted into rather than opted out of, so there is no original for a rule to reach: nothing to patch, nothing to restore, no prototype chain leading back.

- **`Date`.** `Date.now()` and `new Date()` read the system clock. A rule needing a date receives `ctx.today`, fixed for the run.
- **`performance`.** `performance.now()` is a clock under another name, and the one most likely to survive a reviewer thinking "we removed `Date`, so there is no clock."
- **`WeakRef` and `FinalizationRegistry`.** These make garbage-collection timing observable — nondeterminism that does not look like a clock at all.

**Deleted at startup**, because the base objects are not optional:

- **`Math.random`.** Weaker in principle, since deletion can be undone if a reference escapes. Sufficient in practice: a rule that defines its own `Math.random` has written deterministic code, and the engine's entropy source is unreachable.
- **`SharedArrayBuffer`, `Atomics`.** Meaningless without threads; removed to keep the surface small rather than against a known attack.

**Never part of this engine**, asserted rather than assumed: `fetch` and every other network API, `process`, `env`, ambient `fs`, `child_process`, `require`, and timers — `setTimeout` and friends have no meaning in a synchronous single-pass engine.

**`eval` and the `Function` constructor are present**, and cannot be removed: the engine's own script evaluation depends on that intrinsic, so omitting it makes the sandbox unable to run anything at all. This grants a rule no capability it lacks — a rule is already arbitrary code — but it does mean reviewing a third-party rule cannot rely on reading its source alone.

### 6.7 Resource limits

Turing-complete rules can fail to terminate. Three limits, all mandatory, all on by default, none disableable — a rule that hangs a pre-commit hook is indistinguishable from a broken tool.

| Limit | Default | Scope |
|---|---|---|
| `timeouts.rule` | 1 s | One handler invocation — a single `check` call, or a single `reduce` call |
| `timeouts.global` | 15 s | Wall clock for the entire run, across all rules and files |
| `limits.memory` | 64 MiB | Per JavaScript runtime, so per rayon worker |

Both timeouts are configurable in `lanekeep.config.ts`, and `timeouts.global` also via `--timeout`. An individual rule may raise its own invocation budget with a `timeout` field in `defineRule` — the escape valve for a `reduce` that legitimately processes a large corpus, without loosening the default for every rule.

The two levels do different jobs. The per-invocation limit fires fast and **names the culprit**: which rule, which file, which phase. The global limit is the backstop for the case no single invocation is pathological but the aggregate is — a thousand rules each taking 20 ms. Keeping the per-rule default well under the global one means the diagnostic almost always comes from the level that can identify the cause.

### 6.8 Breaching a limit cancels the run

Any limit breach aborts the entire run: exit code `2`, a diagnostic naming the rule, file and phase, and no report.

The alternative — skip the offending rule and continue — is tempting and wrong. A timeout is timing-dependent by nature, so a rule that trips on a loaded machine and not on an idle one would make output vary between runs on identical input. That directly contradicts §11's guarantee that an agent reading the output twice must not see reordering as change, and it would let a partial, silently-incomplete result pass for a clean one. A checker that could not finish must not report that it found nothing.

Mechanically: the per-invocation limit uses QuickJS's interrupt handler; the global limit is a deadline shared across workers, checked by the same handler and between files. Tripping either sets an abort flag that every rayon worker observes at its next file boundary.

**Cache entries for files that fully completed are still committed.** Each entry is independently valid — it records the result of running every rule against that file to completion — and discarding them would mean a corpus that times out on a cold run times out identically on every retry, with no way to make progress. Files that were in flight when the run aborted are not written.

---

## 7. Making it fast

The performance model rests on JavaScript running as rarely as possible. Three gates, in cost order.

### 7.1 Pre-parse gates (no parse, often no read)

Declared per rule under `gates`, evaluated in Rust:

| Gate | Cost |
|---|---|
| `pathMatches` / `pathNotMatches` | No file read at all |
| `fileContains` / `fileNotContains` | One read, substring scan via memchr. No parse. |

The single largest lever available. A rule scoped to `makeStyles` skips parsing every file whose bytes do not contain that string. Gates are pure optimization — removing one changes results not at all, only speed.

### 7.2 The query gate

The tree-sitter query runs in Rust across the parsed tree in a single pass shared by every rule targeting that file. Only matches cross into JavaScript.

This is the decision that keeps the rewrite worthwhile. Dispatching into JS per *node* — the conventional plugin shape — crosses the boundary tens of thousands of times per file. Dispatching per *match* crosses it a handful of times. The difference is two to three orders of magnitude, and it is the entire reason a Rust engine still earns its place once rules are JavaScript.

### 7.3 The cache

A warm run with no changes executes **no JavaScript at all** — every file is a cache hit and handlers are never invoked. This is why the warm budget (§15) stays aggressive even though rule execution is interpreted.

---

## 8. Cache

### 8.1 Key

```rust
key = blake3(
    engine_version_major_minor,   // bump breaks cache intentionally
    host_api_version,             // adding or changing a ctx function invalidates
    grammar_id, grammar_abi,      // per language
    ruleset_hash,                 // hash of all rule module sources in the graph
    config_hash,                  // severity, include/exclude, options
    file_relative_path,           // path gates exist — path is an input
    file_content_hash,            // blake3 of bytes
)
```

Value: `{ violations, facts, suppressions, deps }`.

Three things people get wrong here, all of which are silent-staleness bugs:

- **`ruleset_hash` covers every module in the rule import graph**, not just the entry files. A rule importing a shared helper must invalidate when that helper changes.
- **Relative path belongs in the key.** Path gates make results path-sensitive; a moved file with identical bytes is not a cache hit.
- **Grammar ABI belongs in the key.** A tree-sitter grammar bump changes node shapes and therefore query results.

Suppressions live in the entry because directives are parsed during the per-file pass, and a reduce-phase violation may be reported at a site in a file that was not reprocessed this run. An entry without them would drop the directive and report a suppressed violation on the warm path.

### 8.2 Dependency tracking

`ctx.readFile` makes results depend on files other than the one being checked. Purity is therefore replaced by **tracked effects**: every read is recorded in the entry as `deps: [(path, content_hash)]`, and a cache hit additionally requires every recorded dependency to still hash identically.

This is the standard build-system approach, and it is what allows a rule to cross-reference other files without giving up incrementality.

### 8.3 Storage

Single memory-mapped file under `.lanekeep/cache`, not per-file entries (inode churn dominates at 2k+ files). Writes go to a temporary file committed by atomic rename. Cache is disposable — corrupt or unreadable means full recompute, never an error.

### 8.4 Incremental entry points

- `--since <ref>` — files changed vs a git ref.
- `--staged` — files in the index. Pre-commit hook default.
- Neither flag — full discovery, but warm cache makes it cheap.

---

## 9. Configuration

```ts
// lanekeep.config.ts
import { defineConfig } from 'lanekeep'
import noNumericSizes from './lanekeep/rules/no-numeric-sizes'
import noTypographyInStyles from './lanekeep/rules/no-typography-in-styles'

export default defineConfig({
  include: [
    'apps/*/src/**/*.{ts,tsx}',
    'packages/*/src/**/*.{ts,tsx}',
  ],
  exclude: [
    '**/*.{test,spec}.{ts,tsx}',
    '**/__tests__/**',
  ],

  rules: [noNumericSizes, noTypographyInStyles],

  severity: {
    'lanekeep/no-default-export': 'warn',
    'local/no-numeric-sizes': 'error',
  },

  // Defaults shown. Breaching either cancels the run — see §6.8.
  timeouts: {
    rule: 1_000,     // ms, per handler invocation
    global: 15_000,  // ms, wall clock for the whole run
  },
})
```

Config is a rule module like any other: same loader, same sandbox, same absence of npm resolution. Composition is ordinary TypeScript — a shared preset is a module that exports an array of rules, imported and spread. No bespoke `extends` mechanism is needed, because the language already has one.

`severity` is applied last and overrides whatever a rule declares.

---

## 10. Suppressions

```ts
// lanekeep-ignore-next-line local/no-numeric-sizes reason: legacy API requires exact 44
minWidth: 44,

// lanekeep-ignore-file local/no-primitive-components reason: generated fixture
```

- Rule IDs whitespace- or comma-separated. `reason:` mandatory.
- Directive must be a standalone token, so prose mentioning it doesn't match.
- `--report-unused-suppressions` — hygiene. A suppression whose violation no longer exists is debt.
- Optional `expires: YYYY-MM-DD` — surfaces as a violation past the date, compared against the host-fixed `ctx.today`. Makes "temporary" suppressions actually temporary.

---

## 11. Output

| Format | Purpose |
|---|---|
| `human` | Default. Path:line:col, rule id, message, indented remediation. Colors auto-off when not a TTY or `NO_COLOR`. |
| `json` | Machine-readable, stable schema, versioned. |
| `sarif` | Free GitHub code-scanning integration. |
| `agent` | Token-minimal, remediation-first, grouped by rule rather than by file. Includes rule cards for violated rules only. |

Violations sorted `(ruleId, file, line, column)` always. Deterministic output matters more than usual here: an agent reads it twice and must not see reordering as change. This is also why the sandbox withholds randomness and clock access (§6.6) — a rule cannot introduce nondeterminism even by accident.

**Exit codes:** `0` clean or `--warn-only`; `1` violations; `2` runtime error, which includes a cancelled run — a breached timeout or memory ceiling (§6.8) never exits `0` or `1`, because a checker that could not finish must not be mistaken for one that found nothing.

---

## 12. CLI surface

```
lanekeep init
lanekeep check [paths...]
        [--since <ref> | --staged]
        [--format human|json|sarif|agent]
        [--warn-only] [--profile] [--no-cache]
        [--timeout <ms>]        # global budget, default 15000
lanekeep check --watch          # foreground, incremental, re-runs on change
lanekeep explain <rule-id>      # prints the rule card
lanekeep rules [--json]
lanekeep server                 # M3 — LSP + MCP, launched by editors/agent hosts
```

One-shot is the default and CI runs it unchanged. `--watch` is a foreground loop, not a background daemon. `server` is explicit and separate. The warm cache is what makes one-shot fast; process persistence is an optimization for editors, not a requirement for speed.

---

## 13. Security posture

Rules are executable code. The posture is therefore about **confinement**, not absence:

- **No ambient authority.** Rule code reaches exactly the host functions in §6 and nothing else. `fs`, `process`, `child_process`, network and dynamic import are not restricted — they do not exist in the context.
- **No network.** Ever, in any mode, with no configuration that enables it.
- **Filesystem confinement.** Reads happen only through `ctx.readFile`, only within the project root, with traversal rejected. Writes happen only under `--fix`, only to matched files, only within reported ranges.
- **Bounded execution.** A per-invocation timeout, a 15-second global run budget, and a per-runtime memory ceiling (§6.7). None disableable, and breaching any of them cancels the run rather than degrading to a partial result (§6.8).
- **Determinism by construction.** No clock, no randomness (§6.6).
- Every built-in rule is reviewed by a maintainer.
- Supply chain: npm provenance / sigstore attestations, CI pinned to SHAs, `cargo-deny` + `cargo-audit` in CI, minimal dependency surface. This is a pre-commit-hook-adjacent tool and therefore a target.

**What this does not defend against.** A malicious rule in your own repository can still report misleading violations, read any project file, and consume its resource budget. lanekeep is not a boundary against someone who can already commit to the repository being checked — that person can modify the source and the CI configuration directly. The confinement exists to bound blast radius and to make third-party rule sets reviewable, not to make untrusted code safe to run unread.

---

## 14. One-way doors

Cheap now, breaking changes later. Lock all five before writing much code.

1. **Namespaced rule IDs from day one.** `lanekeep/<id>` for built-ins, `local/<id>` for project-authored. Bare IDs in v1 would break every config file, every suppression comment, and every consumer parsing JSON output when namespaces arrive. This is the expensive one.
2. **The host API is versioned and in the cache key** (§8.1), so adding a `ctx` function invalidates correctly rather than silently serving results computed without it.
3. **Tracked effects from the start.** Retrofitting dependency tracking onto a cache that assumed purity means every existing entry is unsound. `deps` ships with the first cache.
4. **Nodes cross the boundary as handles, never as objects.** Materializing an AST for JavaScript is a decision that cannot be walked back once rules depend on the object shape.
5. **A clean internal `Rule` boundary.** Built-in rules are authored in TypeScript against the same host API as user rules — the strongest available evidence that the API is sufficient. Should a built-in ever need to be reimplemented in Rust for speed, it does so behind the same ID and the same trait.

---

## 15. Performance budget

Targets, gated in CI with regression thresholds:

| Scenario | Budget |
|---|---|
| Cold full run, ~2k files, ~20 rules | < 800 ms |
| Warm run, no changes | < 25 ms |
| Warm run, 1 changed file | < 10 ms |

The cold budget is looser than a pure-Rust engine would allow, and that difference is the honest price of programmable rules: handler invocation and QuickJS interpretation cost real time on every match. The query gate (§7.2) is what keeps that cost bounded to matches rather than nodes.

The warm budgets are unaffected, because a warm run executes no JavaScript at all (§7.3).

These numbers are provisional until M1 measures them. If the cold budget proves unreachable, the levers in order are: better gate usage in built-in rules, bytecode caching across runs, then a faster engine behind the §5.1 trait. Relaxing the budget is the last resort, not the first.

Instrumentation behind `--profile` only, reporting per-rule time split between query matching and handler execution — the split that tells an author whether their query or their code is the problem.

`benches/` runs on a fixed corpus, in CI, with a hard failure on regression beyond threshold.

---

## 16. Milestones

**M0 — walking skeleton.** Workspace, config loading, discovery, tree-sitter TS/TSX, query compilation, the embedded engine with the §6 host API, human + json reporters, `RuleTester`. Acceptance: the built-in rules and a representative set of project-authored rules run end-to-end against the fixture corpus, with snapshot-verified output.

**M1 — speed.** Cache with dependency tracking, `--since` / `--staged`, rayon parallelism, `benches/` in CI with regression gates. Acceptance: budgets in §15 met.

**M2 — completeness.** Full host API surface, SARIF + agent reporters, `explain`, fixes (template-based replacement of a capture, marked machine-applicable vs suggestion), unused-suppression reporting.

**M3 — loops.** `--watch`, then `server` (LSP + MCP).

**M4 — second language.** Python is the cheapest proof that the `Language` trait abstraction actually holds. If adding it requires touching `lanekeep-core`, the abstraction was wrong and it is far better to learn that at M4 than at M10.
