# lanekeep — Architecture

Deterministic, AST-based architectural conformance checking for AI-generated and human-written code.

Not a linter in the ESLint sense. ESLint enforces language-level correctness; lanekeep enforces project-specific conventions an LLM has no way to infer from the code it is shown. Every rule is a codified answer to *"the agent keeps doing this wrong."*

---

## 1. Scope

### v1 goals

- TypeScript / JavaScript (incl. TSX/JSX).
- **Rules are programs, authored in TypeScript** — the same language as the code they inspect. Turing-complete, with no expressiveness ceiling to hit. A rule may also be a WebAssembly component (§4), which is how six of the ten built-ins ship: two written in Rust, and four compiled from the TypeScript they were already written in. TypeScript is the form a project starts from and the one this document is written in, and it is now the form on **both** sides of that door.
- **Rules run sandboxed inside the binary.** An embedded JavaScript engine, or wasmtime for a component, reaching only host functions lanekeep chooses to expose.
- **The hot path stays in Rust.** A rule declares a tree-sitter query; Rust matches it at native speed and calls into TypeScript only on matches.
- Built-in rules ship with the tool and are authored against the same API user rules use.
- One-shot CLI as the default execution model.
- Content-addressed cache with dependency tracking; incremental by file content.
- Fast enough for the inner loop: agents and developers invoke it after every edit.

### Why rules are code, not configuration

An earlier draft of this design made rules declarative data — a query plus a fixed vocabulary of predicates, evaluated entirely in Rust. That bought a trivially safe security posture and a simple cache, at the cost of a hard expressiveness ceiling: a rule the vocabulary could not express had no remedy short of an upstream pull request.

That trade was wrong for this tool. The rules that matter are the ones specific enough that nobody else would ever write them, which is exactly the population a fixed vocabulary fails. And rule authors are TypeScript developers; asking them to learn a bespoke YAML predicate dialect to describe TypeScript is friction with nothing on the other side of it.

So rules are programs rather than declarative patterns, and TypeScript is the form a project starts from. Everything below follows from holding that alongside the performance and determinism goals, rather than trading them away for it. See §6 for the boundary that makes it safe, and §7 for the shape that makes it fast. §4 covers the second authoring form, a WebAssembly component, which changes the language and the engine and none of the reasoning here.

### Explicit non-goals for v1

- No type-aware analysis. Light binding resolution only, exposed as host helpers (§6.4).
- No npm imports from rule code. Rules run in an embedded sandbox, not in Node (§5).
- No autofix in M0 — delivered in M2, §14.
- No LSP/MCP server in M0 — delivered in M3, and `lanekeep server` now speaks both.

### Distribution

Single static Rust binary with the JavaScript engine and wasmtime compiled in — no Node.js required to run lanekeep, even where rules are written in TypeScript, and no toolchain required for a component, which ships as bytes. Shipped via npm (platform packages + thin wrapper, the esbuild/swc/Biome pattern), PyPI (one platform wheel per target, no wrapper — a wheel names its own platform and pip picks by that tag), cargo, and Homebrew — the last from a tap rather than homebrew-core, whose notability requirements a new project does not meet.

A checker that reads Python should be installable by a Python project on its own terms; the alternative was telling a `pip`/`uv` team to install Node. The Linux binaries are built against glibc 2.17 so the `manylinux_2_17` tag those wheels carry is true, and that floor is asserted against the binary rather than assumed — see [`docs/releasing.md`](releasing.md).

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
    lanekeep-core/       # types, discovery, gates, violations, ordering contract
    lanekeep-engine/     # the walker: discovery, gates, parsing, handler invocation
    lanekeep-js/         # embedded engine, sandbox, host API, module loader
    lanekeep-query/      # query parsing + compilation
    lanekeep-nodes/      # the node arena: handles shared by every rule-execution engine
    lanekeep-lang/       # Language trait + registry
    lanekeep-lang-js/    # TS/TSX/JS/JSX grammars + binding resolution
    lanekeep-lang-python/ # Python grammar + binding resolution
    lanekeep-lang-go/    # Go grammar + binding resolution
    lanekeep-lang-rust/  # Rust grammar + binding resolution
    lanekeep-languages/  # the set of supported languages, assembled in one place
    lanekeep-config/     # schema, config loading, canonicalization + hashing
    lanekeep-cache/      # content-addressed store with dependency tracking
    lanekeep-wasm/       # component execution: the WIT host API, wasmtime wiring
    lanekeep-rules/      # built-in rules, embedded at build time (TypeScript modules,
                         #   the two components built from rust-rules/, and the one
                         #   built from four of the TypeScript rules by componentize-js)
    lanekeep-report/     # human, json, sarif, agent reporters
    lanekeep-cli/        # binary
    lanekeep-server/     # LSP + MCP over stdio
    lanekeep-testkit/    # RuleTester: fixture-based snapshot harness
  rust-rules/            # a second Cargo workspace: rule crates authored in Rust
    lanekeep-rule/       #   the SDK they share
  cmd/lanekeep/          # the Go launcher, so `go tool lanekeep` works
  npm/                   # platform packages + wrapper
  benches/
  docs/
```

`lanekeep-engine` exists because the walker cannot live in `lanekeep-core`. Running rules requires the sandbox, and the sandbox is built *on* core — putting the walker there would make the two crates mutually dependent. It cannot live in `lanekeep-cli` either, because `lanekeep-testkit` has to run rules too, and a harness reaching into the binary crate would be a worse coupling. So it sits above the sandbox and below both consumers, and core keeps the types, discovery, gates and the ordering contract.

`lanekeep-testkit` is not optional. Without a `RuleTester` equivalent, community rule contributions are unreviewable.

`lanekeep-languages` is the composition root for languages. Both the CLI and the testkit need to know which languages exist, and no crate below them can hold that answer without inverting the dependency the `Language` trait exists to create.

`packages/lanekeep` holds the TypeScript definitions for the whole host API, plus the `defineRule`/`defineConfig` helpers, so rules are autocompleted and type-checked in the author's editor. They ship *inside* the `lanekeep` npm package rather than as one of their own, because `lanekeep` is the specifier a rule imports from — types under any other name would be types nobody's editor finds.

**It ships runnable JavaScript now, and that is a change of kind rather than of size.** The package was types plus two identity functions, and the sentence here used to be "nothing there runs in Node". Three files under `packages/lanekeep/runtime/` are real code with real tests:

- `host.js` — the `ctx` a rule is written against, assembled over the world's `check-context` and `reduce-context`, plus the deletion of every ambient global `componentize-js` leaves lying around (§13). It runs **inside the component**, not in Node.
- `entry.js` — the seven exports `world rule` declares, over a table of registered rules. Also inside the component.
- `resolve.js` — lanekeep's module resolution rules, ported from `lanekeep-js`'s loader. This one runs **in Node**, at build time, wired into the bundler that compiles a rule ahead of time (§5.3).

So the package now has three audiences: an author's editor, a component's guest, and a build. The `index.js` that was there all along is still the third thing — a tool that *does* load a rule under Node finds something coherent, and its `defineRule` is the same identity function the sandbox provides.

The definitions are asserted against this crate's own registration in `lanekeep-js/tests/host_types.rs`, in both directions. A definition that drifts from the engine is worse than none — it produces confident autocomplete for a method that throws at run time. The runtime half is held to the engine the same way and by the same reasoning: `lanekeep-wasm/tests/js_globals.rs` holds `host.js`'s withheld list to what `lanekeep-js`'s sandbox withholds, as an equality rather than a subset, and `lanekeep-js/tests/resolver_parity.rs` holds `resolve.js`'s refusal messages to `ResolveError`'s own words.

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

A rule declares metadata, a tree-sitter query that gates execution, and a handler invoked once per match. There are two ways to author one, and the rest of the engine treats them alike: **a TypeScript module with a default export**, evaluated in the sandbox of §5, or **a WebAssembly component** exporting the `rule` world of §6.9, executed by wasmtime. `RuleSpec::component` is the single field that decides which engine a rule goes to, and it is set where a rule is described rather than guessed at anywhere downstream.

Neither is privileged. A component is held to the same validation a TypeScript rule is — namespace, card, query, `has-check` — by the same code, and both engines run in one pass over one corpus. Which form a rule takes is invisible to a config: `lanekeep/no-unwrap` names the rule, not its implementation, so a rule migrating from one to the other requires no config change.

**A component's source language is not part of the arrangement, and six of the ten built-ins are components today.** Two are written in Rust and compiled with `cargo component` — `docs/authoring-rust-rules.md` is how one is written. The other four are *the same TypeScript files they were as modules*, compiled ahead of time by `componentize-js` into one shared component (§5.2). That form is the reason the arrangement has to be two ways of shipping a rule rather than "TypeScript rules are modules and everything else is a component": the four moved with their sources frozen byte-for-byte, and the four test files that covered them as modules pass unmodified against them as a component. Which engine runs a rule and which language it was written in became independent questions in that change.

The TypeScript form is the one everything below is written in, because it is the one a project starts from.

```ts
import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-numeric-sizes',
  language: ['typescript', 'tsx'],
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
      line: ctx.line(m.match),
      column: ctx.column(m.match),
    })
  },

  reduce(ctx) {
    const imported = new Set(
      ctx.facts('import').flatMap(f => f.symbols),
    )
    for (const e of ctx.facts('export')) {
      if (!imported.has(e.symbol)) {
        ctx.report({ file: e.file, line: e.line, column: e.column })
      }
    }
  },
})
```

`reduce` receives facts and the discovered file list. It never receives parse trees — that is invariant 1 from §2, and it is what keeps cross-file rules incremental.

Positions travel as plain numbers rather than as an opaque location object, because a fact has to survive JSON to be cacheable. The same constraint is why `reduce` reports at `{ file, line, column }`: there are no nodes in that phase to report at, so the position has to be captured during the per-file pass, while the tree is still there.

`file` is attached by the host, not by the rule, and it is attached last — so a rule that puts its own `file` in a fact cannot make a violation appear to come from somewhere it did not.

A rule sees only its own facts. Reading another rule's would turn a private payload shape into a contract between rules, and would make results depend on the order rules were declared in.

---

## 5. The JavaScript host

One of the two rule-execution engines, and the one every project starts from. It runs the TypeScript form of §4 **as a module, at run time**; §6.9 is the other. Everything in this section is about that engine specifically — a component reaches none of it, and neither the stripper nor the module loader nor QuickJS is on its path at all.

Since §5.2's second half, "TypeScript" and "this engine" are no longer the same set. The same TypeScript can be compiled ahead of time into a component and never meet QuickJS at all, which is how four of the built-ins now run. Read this section as being about a rule that is *loaded*, and §5.2's compiled path as the alternative to being loaded rather than an alternative language.

### 5.1 Engine

QuickJS, embedded via `rquickjs`. Chosen over V8 and Boa for a combination of reasons: it compiles into a static binary at roughly a megabyte rather than tens, it starts in microseconds rather than milliseconds — which matters when the warm-run budget is 25 ms — and its runtime is straightforwardly one-per-thread, which is what rayon wants.

Its weakness is raw throughput: QuickJS interprets, where V8 compiles. That is affordable here precisely because of the query gate (§7) — JavaScript runs proportional to matches, and matches are a tiny fraction of nodes.

**There is no engine trait, and deliberately so.** An earlier draft of this section claimed one; there never was. A trait with a single implementor is speculative abstraction — it fixes the shape of a boundary before anything has pushed against it, and the shape a second engine would actually want is not knowable from the first. What exists instead is containment: every line that knows QuickJS exists lives in `lanekeep-js`, behind `Sandbox` and the host context, and no other crate names `rquickjs` at all. Swapping the engine means rewriting one crate against an interface its callers already use, which is the work a trait would have front-loaded on a guess.

M1 measured handler execution at roughly three times query matching on a synthetic corpus (§15), which is real but not the dominant cost — the dominant cost is starting the engine and evaluating rule modules, which is why that is now done lazily. Nothing so far argues for a different engine.

### 5.2 TypeScript

Rule modules are TypeScript. Types are stripped before evaluation — a syntactic transform, not a type check. Rules are type-checked in the author's editor against the types shipped in `packages/lanekeep` — see §3. lanekeep itself never type-checks, because doing so would mean shipping a TypeScript compiler and paying its cost on every run.

This is a deliberate division: the authoring experience is fully typed, the runtime is not.

**Stripping overwrites type syntax with spaces, in place.** Every surviving byte keeps its original offset and newlines inside a blanked span are preserved, so a line and column in the generated JavaScript is the same line and column in the author's source. A stack trace from a rule that threw therefore points at the original TypeScript with no source map to generate, ship, parse, or get subtly wrong — **on this path.** The compiled path below has no such property and does ship a map; §6.9 says what it costs there.

The stripper reuses the TypeScript grammar already present for §7.2, so it costs no additional dependency. A full TypeScript transformer would handle every construct but roughly triples the dependency graph of a tool that runs as a pre-commit hook — see §13.

**Four constructs are rejected rather than stripped**, because they generate runtime code and so have no type syntax to remove: `enum`, `namespace`, decorators, and constructor parameter properties. Each is rejected with the plain alternative named. Rule modules are small and self-contained, and emitting JavaScript that silently means something else would be far worse than refusing.

Stripping is verified rather than trusted: the output is re-parsed as JavaScript, and a syntax error is reported as a bug in lanekeep rather than in the rule. That turns a whole class of subtle stripping errors into a loud failure at the point of the mistake.

#### The other path: the same TypeScript, compiled ahead of time

Everything above happens at run time, once per run, inside the binary. A TypeScript rule can instead be compiled *before* the binary ever sees it: `jco componentize` bundles the rule and lanekeep's own runtime with rolldown, evaluates the result into a StarlingMonkey build with `wizer`, and emits a WebAssembly component exporting the `rule` world of §6.9. The output is an ordinary component and nothing downstream knows which language produced it.

**Four of the built-ins ship that way, from sources that did not change.** `no-default-export`, `no-restricted-imports`, `no-circular-imports` and `no-unused-exports` are byte-for-byte the files they were as modules; what moved is which engine runs them. One component hosts all four, because the engine is the cost and the rules are rounding error on it — see §6.9 for the matched pair, and note that this is what forced §6.9's rule *index* to exist.

Three things are worth knowing before writing a rule for this path.

**It supports three of the four constructs the stripper rejects, and refuses the fourth loudly.** The stripper blanks type syntax and so cannot express anything that generates code; a bundler performs a real transform, so `enum`, `namespace` and constructor parameter properties all compile here. Measured 2026-08-11 with jco 1.27.0 and componentize-js 0.22.0, and measured as *lowered* rather than merely parsed — each was read back at build time by the registration that validates a rule, so an untransformed one would have failed. **Decorators are the exception**: rolldown passes the `@` through untouched and StarlingMonkey refuses it while the component is being built, with `SyntaxError: illegal character U+0040` and the offending line. That is a build failure naming the character, which is a good failure; it is not, however, the plain alternative the stripper names, so a rule using decorators is refused by both paths for different reasons and with different messages.

**It builds outside every gate, and the artifact is committed.** `just typescript-builtins` needs Node, and `just check` must pass on a machine that has none — so the component is a committed binary reaching the binary through `include_bytes!`, exactly as the Rust ones are. What that costs is a staleness check: `componentize-js` is **not** byte-reproducible (two builds of one unchanged tree differ in ~2.9 MB), so the "rebuild and see whether `git status` is clean" check that covers the Rust artifacts cannot work here. `crates/lanekeep-wasm/tests/fixture_currency.rs` digests the *sources* instead — every rule source, the runtime, the bundler configuration, `package.json` and `package-lock.json` — and reddens when one moves without a rebuild.

**A host call from it is expensive, and §15.1 has the number.** About 110× a QuickJS one, measured against the same rule. That is not the canonical ABI, which a Rust component pays at 1.1×; it is a JavaScript engine interpreting inside a sandbox it cannot emit machine code from. It does not change what a rule may do or what it computes, and it is the reason this path is offered rather than mandated.

### 5.3 Module loading

Rules may import from each other and from `lanekeep`. A custom ES module loader resolves:

- `lanekeep` → the host-provided module (`defineRule`, `defineConfig`, helper types)
- Relative specifiers → other files under the project's rule directory

Anything else fails at load time with a clear diagnostic. There is no `node_modules` resolution, no `require`, and no bare-specifier resolution to npm packages.

**These rules are enforced twice, in two languages, deliberately.** `lanekeep-js`'s loader enforces them at run time for a rule that is loaded. A rule compiled ahead of time (§5.2) has no run time left to enforce anything in — the bundler decides what every import means before the component exists — so `packages/lanekeep/runtime/resolve.js` is the same rules again, in JavaScript, wired into rolldown's `resolveId` and `load` hooks. Either both agree or a rule means one thing when loaded and another when compiled, which is the worst available outcome for a confinement boundary.

The duplication is deliberate and the drift is what is guarded against, not the duplication. Three things do the guarding, and they are not interchangeable:

- **The refusal messages are `ResolveError`'s own, word for word**, and `lanekeep-js/tests/resolver_parity.rs` reads both files as text to say so. A rule author who hits one refusal and searches for the other has to find it. Message parity is also the cheapest available canary that the *rules* drifted, since a changed rule almost always changes what it says.
- **`load` re-confines rather than trusting `resolveId`.** `RuleRoot::read` re-canonicalizes for the same reason: the operation that actually touches a file should be the one enforcing the boundary, or the guarantee holds only while every caller goes through the resolver first. A bundler will hand `load` an id any plugin resolved, so that applies with more force here.
- **The port's tests are `loader.rs`'s tests**, one for one, plus the cases only a port can get wrong — component-wise containment, which Rust gets free from `Path::starts_with` and JavaScript does not, and which a `startsWith` on the root string would silently give away.

Two departures are documented at their definitions rather than left to be discovered. `confine` normalizes its argument, and its *lexical* gate accepts either spelling of the root, because a caller building a path from a configured root on macOS gets `/var/…` where `realpath` says `/private/var/…`. The canonical gate — the boundary itself — is untouched by both.

### 5.4 Compilation and reuse

Rule modules are compiled to QuickJS bytecode once per run, then instantiated in each worker's context. Compilation cost is paid once regardless of corpus size; per-worker instantiation is cheap.

---

## 6. The host API — and the boundary that replaces "no code execution"

The previous design's security posture was "rules are data, so there is nothing to execute." That is gone. What replaces it is narrower but still strong: **rules are code, but the only things they can reach are the functions lanekeep hands them.** There is no ambient `fs`, no `process`, no network, no dynamic import. Those globals are not restricted — they are absent.

This is a stronger position than a conventional plugin system offers, where a plugin inherits the full authority of the host process.

**It is one API with two spellings.** The tables below give the JavaScript one, which is what a TypeScript rule calls. A component reaches the same functions as methods on `check-context` and `reduce-context`, declared in `crates/lanekeep-wasm/wit/world.wit` — `ctx.report(node)` is `ctx.report(node)` there too, in kebab-case. §6.9 covers what a component has that a TypeScript rule does not, and it is only what the difference in form forces. The two are not generated from one source and that gap is recorded in the world's own header; the *absence* claims below are structural in both, because a component reaches exactly what its world declares.

**Two spellings, and — for the length of the coexistence window — two places the JavaScript one is built.** A rule compiled ahead of time (§5.2) is written against the same `ctx` as a loaded rule and reaches it through a different implementation: `lanekeep-js`'s `host.rs` installs it on a QuickJS object, and `packages/lanekeep/runtime/host.js` assembles it in the guest over the world's resource methods. The same rule source runs against both, which is what makes the pairing testable rather than aspirational — the four migrated built-ins' test files pass unmodified across the move — and `host.rs` is the specification where the two disagree, stated in `host.js`'s own header.

The window closes when the last rule is loaded rather than compiled, and at that point the QuickJS half leaves with it, along with `HOST_API_VERSION` (§14). Until then a `ctx` function is added in three places or it is added wrong: the world, the guest shim, and the QuickJS host. Three shapes differ in ways worth knowing before writing one — a property in the published API is a *method* on `check-context`, so `host.js` fronts `filePath`, `fileText` and `root` with memoized getters (a rule reading `ctx.fileText` twice pays one crossing, and one that never reads it pays none), while `today` is a getter that deliberately re-crosses every time, because the host has to observe *that the date was read* to know which files depend on it.

### 6.1 Reporting

| Function | Notes |
|---|---|
| `ctx.report(node, message?)` | Emit a violation. |
| `ctx.report(node, { message?, fix? })` | The same, with a replacement offered. `fix` is `{ node, text, safe? }`. |
| `ctx.loc(node)` | `{ file, line, column }` |

### 6.2 Tree navigation

| Function | Notes |
|---|---|
| `ctx.text(node)` | Source text of a node |
| `ctx.kind(node)` | Node kind |
| `ctx.parent(node)` / `ctx.children(node)` | |
| `ctx.ancestors(node)` | Lazy, outermost-last |
| `ctx.closestAncestor(node, query)` | Nearest ancestor the query matches *at*; returns its captures, or `undefined` |
| `ctx.querySubtree(node, query)` | Run a query scoped to a subtree |

`closestAncestor` matches *at* an ancestor, not *within* one: the query runs rooted at each ancestor in turn and a match counts only if it captured that ancestor. Without that rule, a query matching anything anywhere inside would make the outermost ancestor the answer every time, which is never what a rule walking upward wants. It returns `undefined` rather than an empty object when nothing matches, because an empty object is truthy and `if (!ctx.closestAncestor(...))` would silently take the wrong branch.

Queries passed to either are compiled once per file and cached by source, including the failures — a rule calling `querySubtree` inside a handler calls it once per match with the same string, and compiling per call would make the second-cheapest operation in the host the most expensive one.

Nodes cross the boundary as opaque integer handles into the Rust-side tree, never as materialized JavaScript objects. Materializing an AST is the cost that makes native tooling with JS plugins slow; the query gate plus handle-passing avoids paying it.

### 6.3 File and path

| Function | Notes |
|---|---|
| `ctx.filePath` | Path relative to project root |
| `ctx.fileText` | Full source text |
| `ctx.readFile(path)` | **Tracked.** Records the read as a cache dependency (§8.2). Returns the text, or `undefined` if nothing is there. Confined to the project root. |
| `ctx.fileExists(path)` | Tracked identically. True for anything present, text or not. |

Absence is an ordinary answer, not an error: a rule asking whether a config is present should not have to catch to find out. Three things *are* errors, because each is a bug in the rule rather than a fact about the project — a path that escapes the root, an absolute path, and reading something that is not UTF-8 as text.

Both are per-file only. A reduce phase has no `readFile`: its reads would be run-level dependencies, and recording them in a per-file cache entry would attribute them to whichever file happened to be checked last.

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
| `ctx.emitFact(fact)` | Per-file phase only. Needs a non-empty string `kind`; must survive `JSON.stringify`, because facts are cached. |
| `ctx.facts(kind?)` | Reduce phase only. This rule's facts, in `(file, emission)` order, each with `file` attached. Omit `kind` for all of them. |
| `ctx.files` | Reduce phase only. The discovered file list. |
| `ctx.report(at, message?)` | Reduce phase form. `at` is `{ file, line, column }` — there are no nodes here. |

The split is enforced, not conventional. `emitFact` does not exist in the reduce context and `facts`/`files` do not exist in the per-file context, because a `check` that could read the corpus would make a file's result depend on files other than itself — and caching that result against its own content would then be unsound.

### 6.6 What is deliberately absent

Absence is achieved two ways, and the first is much stronger.

**Never installed.** The engine's optional intrinsics are opted into rather than opted out of, so there is no original for a rule to reach: nothing to patch, nothing to restore, no prototype chain leading back.

- **`Date`.** `Date.now()` and `new Date()` read the system clock. A rule needing a date receives `ctx.today` — a `YYYY-MM-DD` string, fixed once for the run in UTC, so two files checked a millisecond apart cannot disagree about what day it is and a deadline does not move with the reader's time zone. It is a *date*, not a clock: nothing in the sandbox can observe time passing.
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
| `timeouts.global` | 15 s | Wall clock for one phase of guest execution — see below |
| `limits.memory` | 64 MiB | Per JavaScript runtime, so per rayon worker |

Both timeouts are configurable in `lanekeep.config.ts`, and `timeouts.global` also via `--timeout`. An individual rule may raise its own invocation budget with a `timeout` field in `defineRule` — the escape valve for a `reduce` that legitimately processes a large corpus, without loosening the default for every rule.

**`timeouts.global` bounds a phase, not the process, and there are two phases that run guest code.** Config load starts a clock of its own for the `metadata`, `configure` and probe calls a component answers at load time, and the run starts a second one. A configured 15 s therefore permits up to 30 s of wall clock across a single invocation, and a third phase would make it 45. This is not a break of the limits invariant — each clock still cancels the run outright rather than degrading it, and the pattern predates components, since a `lanekeep.config.ts` has always evaluated guest code before any run clock existed. It is stated here because the number reads as a bound on the process and is not one.

**One number reaches both phases, and getting that wrong was the interesting failure.** `--timeout` is resolved before the config is loaded and handed in, so the config-load clock and the run clock are built from the same value. They were not always: the flag used to be applied to the loaded `Config`, one statement after config load had already instantiated, configured and read `metadata` from every component under whatever the config file said. A component whose `configure` overran therefore failed with a message ending "raise it with `--timeout`" — and raising it changed nothing, because the phase that breached had finished before the flag was read. That is the same shape as the original `--timeout` bug recorded in `AGENTS.md`, recurring in a phase that did not exist when it was first found, which is why a test for it has to **assert the raise**: a test that only lowers a budget passes against a budget nothing applies.

What is still per phase is the *clock*, not the number. Anyone wanting a true process bound should thread one clock from the caller rather than starting one per phase; the reason that is not done already is that config load is reached from callers that never start a run at all.

The two levels do different jobs. The per-invocation limit fires fast and **names the culprit**: which rule, which file, which phase. The global limit is the backstop for the case no single invocation is pathological but the aggregate is — a thousand rules each taking 20 ms. Keeping the per-rule default well under the global one means the diagnostic almost always comes from the level that can identify the cause.

### 6.8 Breaching a limit cancels the run

Any limit breach aborts the entire run: exit code `2`, a diagnostic, and no report. The diagnostic names the rule, file and phase whenever there is one to name — which is every breach except a global budget noticed between files, where nothing was executing and naming a rule would blame the wrong thing.

The alternative — skip the offending rule and continue — is tempting and wrong. A timeout is timing-dependent by nature, so a rule that trips on a loaded machine and not on an idle one would make output vary between runs on identical input. That directly contradicts §11's guarantee that an agent reading the output twice must not see reordering as change, and it would let a partial, silently-incomplete result pass for a clean one. A checker that could not finish must not report that it found nothing.

Mechanically: the per-invocation limit uses QuickJS's interrupt handler for a TypeScript rule and wasmtime's epoch interruption for a component one. The global limit is a deadline shared across workers, read by both of those *and* by the walker before it starts each file — one clock, three places that ask it. The third is not redundant: both of the others only run while a handler does, and a run spends most of its time reading, parsing and matching, so a corpus of cheap invocations would otherwise overrun the budget without anything ever being asked to stop. Whichever notices, the run ends; a breach the walker noticed names no rule, because none was executing.

**Cache entries for files that fully completed are still committed.** Each entry is independently valid — it records the result of running every rule against that file to completion — and discarding them would mean a corpus that times out on a cold run times out identically on every retry, with no way to make progress. Files that were in flight when the run aborted are not written. An aborted run also **merges rather than prunes**: pruning is what ages a deleted file out and is sound only for a run that saw the whole corpus, so a run that stopped early would otherwise age out every file it never reached and leave the next run colder than the one that failed.

**What the global budget spans is a decision, and it has been moved twice.** It was started below component compilation, then above it — one clock rather than two, so that a run could not grant each phase the whole budget — and then below it again, which is where it is. The last move is the one with a reason worth keeping: compiling a component is *host* work whose duration depends on whether `.lanekeep/components` already holds the precompiled artifact, so charging it to the run budget makes a cold run and a warm run over identical input take **different exits** — abort on one, report on the other. The determinism tuple of §8.1 contains no compile cache, and a run's verdict may not depend on a term that is not in it. The four migrated built-ins are what made this reachable rather than theoretical: one 12.4 MiB component takes seconds to compile against a 15 s default, where every component before it took tens of milliseconds.

Compilation is not therefore unbounded. It has a budget of its own, generous and separate, whose breach names compilation rather than a rule — a run that stalls compiling has to say so, and blaming whichever rule was next is the diagnostic this whole section exists to avoid. Moving compilation out of the run clock does not reinstate the two-clocks problem the earlier move fixed, because the two phases that move was about are both *guest* execution: taking host work off the one clock is not the same as putting a second clock over guest work.

Both halves became true in the same release and neither had been true before it, which is worth stating because this section has a history of describing behavior nothing implemented. The *commit* half this section always required and nothing did: `run_files` returned on the first error before it reached the save, so an aborted run wrote no entry at all, and a corpus that overran its budget would have been stranded cold forever the moment that budget started being enforced. The *no-prune* half is doctrine added alongside that fix, and it could not have been stated earlier — there was no save whose pruning behavior anyone had to decide. `lanekeep-engine`'s `an_aborted_run_still_commits_the_files_that_finished`, `an_aborted_run_does_not_prune_what_it_never_reached` and `a_full_run_still_prunes` are what hold the three claims above, and a claim added here without one is a claim in the position these were.

### 6.9 The component surface

A component rule is a WebAssembly component exporting the `rule` world. It reaches the same host API as §6.1–6.5 and is subject to the same limits as §6.7 — enforced by wasmtime's epoch interruption where a TypeScript rule is enforced by QuickJS's interrupt handler — and its absences are §6.6's, structurally: it imports exactly one interface, `lanekeep:host/types`, so there is no wall clock, no filesystem and no entropy to remove, because none was ever bound. A component built for `wasm32-wasip1` imports eleven instances rather than one — the host interface plus ten WASI ones — and is refused at load. Two of those three categories are among them, a wall clock and a filesystem; `wasi:random` is not imported, so the entropy an unconstrained build *could* reach is one this particular artifact does not.

**A component hosts a *list* of rules, and every export but `rules` takes an index into it.** A module is one rule because a file is one module; a component is a compiled program, and the program can be far larger than the rules built on it. Measured 2026-08-10 against a JavaScript engine compiled to a component: one rule is 12.34 MiB and three further rules with real bodies add 9,702 bytes, so one component per rule would ship four migrated built-ins as 49.37 MiB where one shared component is 12.34 MiB. The generalization also buys something for the target that did not need it — one Rust crate can ship a family of related rules in one 26 KB artifact — and a rule authored alone is a ruleset of one that pays a `u32` for the arrangement.

**One instance serves every rule its component hosts, and that is the point of the index rather than a detail of it.** Artifact bytes and resident bytes are different quantities: a shared artifact saves nothing at run time if each worker instantiates it once per rule, and under the 64 MiB per-worker ceiling — which sums across every linear memory in a store — four copies of a 12.34 MiB engine is a breach rather than merely waste. So `lanekeep-wasm`'s instantiation bound is per (worker, **component**), not per (worker, rule). The one case that cannot share is two rules naming the same *index* of one component, because a guest holds one configuration per index and the second `configure` would overwrite the first; a config may legitimately do that — `["./r.wasm", {"rule": "./r.wasm", "options": {…}}]` is how a rule is used bare and configured in one run — so those get an instance each.

**Sharing an instance shares everything the guest keeps beyond its per-index configuration, and an author has to be told so.** A rule authored alone is a program with its own memory; a rule sharing a component with three others is a program those three are also running in. Module-level mutable state — a cache keyed on a path, a counter, a lazily built table — is therefore visible across the rules of one component, and *when* each rule sees it depends on the order rayon happened to hand a worker its files. That is not a bug in the arrangement; it is what one instance means. But it is a way to write a rule whose output depends on scheduling, which §11's ordering guarantee forbids, and neither the world nor `docs/authoring-rust-rules.md` warned about it for the first two components because both hosted exactly one rule.

The rule to write against is narrow and worth stating as a rule rather than as advice: **state that outlives a `check` call must be derivable from that call's inputs.** A memo keyed on the file path is fine — recomputing it gives the same answer, so being handed a populated one is indistinguishable from being handed an empty one. A counter of how many files have been seen is not, and neither is anything a rule reads in `reduce` that `check` wrote to a variable rather than emitting as a fact. Facts exist for exactly that hand-off and are per file by construction; the `two-rules` fixture in `lanekeep-wasm/tests/fixtures/` models the pattern that stays correct when one instance serves several rules.

It has five exports a TypeScript rule has no need of, and each exists because a component cannot do what a module does:

| Export | Why it exists |
|---|---|
| `rules() -> list<string>` | A module *is* the rule; a component has to say which rules it hosts. Ids rather than metadata, and the split is load-bearing: `configure` must run before metadata is read, because a factory rule's card and query are produced by applying the factory to its options — but to configure rule *i* you must know *i* exists. A rule's id cannot depend on its options, because the id is how a config names the rule, so ids enumerate before configuration and everything else is read after it. |
| `metadata(rule) -> rule-metadata` | A TypeScript rule's id, languages, severity, card, query, gates and timeout live in its `defineRule` call, and evaluating the module is how they are read. Nothing can evaluate a component to read a literal, so it answers for itself — once, at config load. |
| `configure(rule, options-json) -> result<_, string>` | A JavaScript factory closes over its options. A component cannot close over a host-supplied value, so options cross as JSON data, once per instance, before any handler runs. A component that refuses its options fails config load with its own message, which is a refusal rather than a trap. |
| `has-check(rule) -> bool` | Extraction records whether a TypeScript rule's `check` is callable, because a misspelled handler would otherwise load cleanly and never fire. A component is asked the same question rather than taken at its config's word. |
| `has-reduce(rule) -> bool` | The same, for the cross-file phase. |

`configure` is a permanent expressiveness difference and not an implementation gap: a factory can be handed a function or a regular expression, and JSON cannot carry either. That is the cost of a boundary that serializes, and it is paid deliberately.

**`check` and `reduce` return `result<_, rule-error>`, which is a way to fail *gracefully* and not a second spelling of a trap.** A guest that traps loses everything it knew: measured 2026-08-10, an uncaught JavaScript throw inside a component reaches the host as `wasm trap: wasm 'unreachable' instruction executed`, with the thrown value's message, its type and its whole stack gone, because there was nowhere for any of it to go. `rule-error` carries a message and a list of `stack-frame`s in whatever space the guest was compiled to, which is what a host can then remap through the component's source map before a user reads one. A Rust guest that panics still traps and is still handled as a trap; `frames` is empty for a language with no stack to offer. Both spellings cancel the run — what differs is what the run can say afterwards.

The world's bytes are a cache-key input (§8.1, `host_api_hash`), so widening this surface invalidates every cached result — the same property `HOST_API_VERSION` gives the JavaScript half, except that this one needs nobody to remember.

`docs/authoring-rust-rules.md` is the playbook for writing one.

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
    format_version,               // the on-disk encoding
    engine_version_major_minor,   // bump breaks cache intentionally
    host_api_hash,                // what a rule may reach: the ctx surface, the WIT world,
                                  //   and anything bound beside that world
    wasm_compile_env_hash,        // how a component is compiled: wasmtime's own
                                  //   precompile-compatibility hash
    every (grammar_id, grammar_abi) in the registry, sorted
    ruleset_hash,                 // rule module sources in the graph, and component bytes
    config_hash,                  // severity, include/exclude, options
    file_relative_path,           // path gates exist — path is an input
    file_content_hash,            // blake3 of bytes
)
```

Every field is length-prefixed before hashing. Without that, `("ab", "c")` and `("a", "bc")` hash alike and two genuinely different runs share a key — the one failure a cache must not have.

**A rule declares which languages it targets, and the file decides which grammar parses it.** `language` accepts one or several and defaults to `['typescript', 'tsx']`; a rule does not run on a file whose language it does not name, and the query is compiled once per language against that grammar. Choosing the grammar from the *rule* instead would parse a `.tsx` file with the TypeScript grammar, turning every JSX element into an `ERROR` node — and a query matches nothing inside one, silently, which on a React codebase means most of the code goes unchecked with no diagnostic.

Grammars enter the key as the **whole registry**, not the one language a given file used. A file's rules can involve more than one grammar, and working out which is harder to get right than accepting that a tree-sitter bump invalidates everything. That over-invalidates by the files using the other languages, which costs a recompute.

`host_api_hash` covers both engines' surfaces. QuickJS's half is still `HOST_API_VERSION`, a constant in `lanekeep-js` that nothing bumps automatically: a `ctx` function added without bumping it serves results computed by a build where the function did not exist — the rule could not have called it, so its verdict was reached without evidence it would have used. The component half is a content hash of `crates/lanekeep-wasm/wit/world.wit`, the file every binding is generated from, so that half needs nobody to remember. Folded in beside it is `lanekeep_wasm::EXTERNAL_BINDINGS`, which declares every interface the host binds *outside* that world — empty today, and not optional the day one is added: a Go rule's map iteration order is decided by whatever fixed entropy source `wasi:random/random` is bound to, so changing that source changes which violation it reports with the world, the component and the config all identical.

`wasm_compile_env_hash` is `wasmtime`'s own `Engine::precompile_compatibility_hash`, read off an engine built from `lanekeep-wasm`'s one configuration. A precompiled `.cwasm` records the tunables it was compiled under and `wasmtime` refuses one that disagrees, so those tunables decide whether a component runs at all; the ones that survive that check still decide what the guest computes, because `MEMORY_RESERVATION` and `MEMORY_GUARD_SIZE` together decide whether Cranelift elides bounds checks. It is taken from `wasmtime` rather than listed here because `check_tunables` compares twenty-six fields, three of which are lanekeep's constants and twenty-three of which move with the `wasmtime` version, the target triple *and the resolved feature set* — Cargo's feature unification can move `concurrency_support`, `recording`, `memory_reservation` or `memory_init_cow` without the version moving.

It is a compilation environment and **not** a runtime, which is why it is not named one. Settings that live entirely host-side are outside it on purpose: the memory ceiling is enforced by a resource limiter the compiled code knows nothing about, and the epoch tick interval only changes *when* a breach is noticed. Those are budgets, and §6.8 already says a budget cancels a run rather than changing its answer — so neither belongs in a key.

Value: `{ violations, facts, suppressions, deps }`.

Three things people get wrong here, all of which are silent-staleness bugs:

- **`ruleset_hash` covers every module in the rule import graph**, not just the entry files. A rule importing a shared helper must invalidate when that helper changes. The module loader records what it actually read, because nothing else in the system knows the helper was involved.

  It hashes module **source bytes**, not a canonicalized form. An earlier draft required canonicalization so that reformatting would not invalidate while editing a regex would — which was achievable when rules were declarative data and canonicalizing meant normalizing a parsed value. Canonicalizing arbitrary TypeScript would mean shipping a formatter and committing to its output forever. So reformatting a rule *does* invalidate its cached results. That is over-invalidation, costing a recompute; the opposite error — serving results computed by code that no longer exists — is the one this section exists to prevent, and the two are not symmetric.

  It also covers **every rule component's bytes** alongside the modules — a component is the code a rule is made of exactly as a module is. Its bytes and not its resolved path, which is absolute: hashing the path would throw a cache away for moving a checkout, and which component a rule *names* is `config_hash`'s through the specifier. Each component's bytes are length-prefixed, and the length is the whole of the delimiting: a `.wasm` is arbitrary binary, so any byte used as a separator could appear inside one, and without the length two different rulesets can concatenate into the same byte sequence.

  **A component hosts a list of rules, so the component fold is not the rule fold.** Every *distinct* component contributes its bytes once, ordered by those bytes; then every *rule* contributes which of those components it runs in, which of that component's rules it is, and the options it was configured with. Distinctness is by content, so two references to one artifact by different paths are one program and one path read twice across a rewrite is two. Folding the bytes once per rule instead would be quadratic in a component's rule count — four rules on a 12.34 MiB component would fold 49 MiB — and could not tell "two rules of one component" from "one component named twice", because both are the same bytes twice. A rule names its component by position in that sorted list rather than by a digest of it, so the component fold stays the only place the code reaches the key and the length prefix above stays the only thing delimiting one program from the next.

  **For a compiled TypeScript rule the key sees the artifact and never the sources, and that is a real difference rather than a restatement.** A rule loaded as a module reaches `ruleset_hash` through its source bytes, so editing it invalidates immediately. The same rule compiled into a component (§5.2) reaches the key through the *component's* bytes, which do not move when the TypeScript does — they move when someone rebuilds. So a stale artifact produces a perfectly correct key over the wrong program: every cached result is consistent, reproducible, and computed by a rule that no longer exists in the tree. Nothing in the key can detect this, because the sources are not in it and must not be — hashing them would invalidate a cache for an edit the shipped artifact does not contain. What closes it is a gate rather than a key: `fixture_currency.rs` digests every source a committed component is built from and reddens when one moves without a rebuild.

  **It folds bytes that were already read, rather than reading the paths itself**, which is the property the module half has for free — the loader records what it consumed. A component is read exactly once per run, when the configuration is loaded: those bytes answer `metadata`, those bytes are folded here, and those bytes are what executes. Reading at each of the three points would let a file that changed in between describe one rule, key a second and run a third, with every check passing.

  This is why **absence is not encoded here**. An earlier version folded a present/absent marker per component, so that a missing component and a present one could not share a key, on §8.2's rule about absent files. Carried bytes cannot represent absence — and do not need to: a component that cannot be read fails configuration loading, so the run whose key would have been wrong never happens. The case is deleted rather than given a wrong encoding, which is stronger than the marker was.

- **`config_hash` is canonicalized properly**, because configuration values genuinely are structured data. The severity map is ordered, so writing the same entries in a different order hashes the same, and `include`/`exclude` are sorted, since reordering globs changes nothing about which files are selected.

  **A rule's options reach the key as data, on the path that knows them.** That reads as obvious and was not true once: a `lanekeep.json` rule's options were interpolated into the generated entry module as a factory-call argument, and `Sandbox::eval_module` hands that module straight to the engine without going through the loader — so the loader, which is what `ruleset_hash` folds, never saw them. Editing `{"rule": "x", "options": {"limit": 1}}` to `{"limit": 2}` produced two identical keys and a warm run kept answering the previous configuration. The general fact is worth more than the instance: **a value that exists only in generated entry-module source is in neither hash.** A `lanekeep.config.ts` was unaffected, because its options live in the config module's own source, which the loader did read — which is also why every test of this property passed against the bug.
- **Relative path belongs in the key.** Path gates make results path-sensitive; a moved file with identical bytes is not a cache hit.
- **Grammar ABI belongs in the key.** A tree-sitter grammar bump changes node shapes and therefore query results.

Suppressions live in the entry because directives are parsed during the per-file pass, and a reduce-phase violation may be reported at a site in a file that was not reprocessed this run. An entry without them would drop the directive and report a suppressed violation on the warm path.

### 8.2 Dependency tracking

`ctx.readFile` makes results depend on files other than the one being checked. Purity is therefore replaced by **tracked effects**: every read is recorded in the entry as `deps: [(path, content_hash)]`, and a cache hit additionally requires every recorded dependency to still hash identically.

This is the standard build-system approach, and it is what allows a rule to cross-reference other files without giving up incrementality.

**Absence is a dependency.** A rule told that `tsconfig.json` does not exist has depended on that answer as much as one that read it, so a miss is recorded with a null hash rather than not recorded at all. Skipping it produces a cache that is correct on every test anyone thinks to write and wrong on the one case that matters: adding a file changes nothing until something unrelated invalidates the entry.

**Reads are memoized within a file.** Reading the same path twice returns the same bytes even if something rewrites it in between. Otherwise a rule could see a file change under it and report differently on two runs over identical input, and the entry would record one of the two hashes with no way to say which answer used it.

Dependencies are keyed by the file whose result they affect, not by the run, and each file's reads are collected independently — so no ordering between files can move a dependency onto the wrong entry.

### 8.3 Storage

Single file under `.lanekeep/cache`, not per-file entries (inode churn dominates at 2k+ files). Writes go to a temporary file committed by atomic rename, so a reader sees the whole previous cache or the whole new one and never a torn write. Cache is disposable — corrupt, truncated, or written by a different build means full recompute, never an error.

The file is **read whole rather than memory-mapped**, which is a deliberate change from this document's original plan. The mapping APIs require `unsafe`, and the workspace denies `unsafe_code`; trading a lint that holds everywhere for a performance claim nothing has measured would be the wrong way round. A whole-file read is one sequential I/O of a few megabytes. If a benchmark ever puts it on the critical path, that is the moment to revisit both decisions together — not before.

The encoding is hand-written and length-prefixed. Decoding is total: every path returns "not an entry" rather than failing, because a cache that can break a run is worse than no cache. One damaged entry discards the whole file, so which results survive never depends on which byte was damaged.

The stored bytes are a function of the entries alone — insertion order does not leak — so a run over unchanged input rewrites an identical file.

### 8.4 Incremental entry points

- `--since <ref>` — files changed vs a git ref, including untracked ones. A file you just created is a file you just changed.
- `--staged` — files in the index. Pre-commit hook default, and not the same as the working tree: what is about to be committed is what should be checked.
- Neither flag — full discovery, but warm cache makes it cheap.

Both are **intersected with discovery** rather than used in place of it, so `include` and `exclude` stay in force. A selection that overrode them would let `--staged` check a vendored directory the config had deliberately excluded.

Both **skip cross-file rules**, and say so on stderr naming the rules that did not run. A reduce phase consumes facts from every file, so running one over a subset does not give a smaller answer — it gives a wrong one. `no-unused-exports` over three changed files would report every export in them as unused, because the importers were never looked at.

Computing them properly would mean processing the whole corpus, which is what the flag exists to avoid. Skipping is the only option that is both fast and never wrong. Staying quiet about it is not an option at all: a rule that silently stops running turns a clean result into a false "fixed".

A ref that does not resolve is an error. Checking everything instead would be a surprising amount of work done silently; checking nothing would look like a clean run.

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

A `lanekeep.config.ts` is a rule module like any other: same loader, same sandbox, same absence of npm resolution. Composition is ordinary TypeScript — a shared preset is a module that exports an array of rules, imported and spread. No bespoke `extends` mechanism is needed, because the language already has one.

`severity` is applied last and overrides whatever a rule declares.

### 9.1 `lanekeep.json`, and why it no longer shares a mechanism

A project that does not write TypeScript can say which rules it wants in `lanekeep.json`, listing rules as `"lanekeep/no-package-init"` or `{ "rule": "...", "options": { ... } }`. It is what `lanekeep init` scaffolds.

It was originally *compiled* into the entry module the TypeScript path produces and handed to the same loader, so that nothing downstream knew which format a config came from and the two could not drift. That was deliberate and it is gone: the JSON path is now parsed, validated and resolved in Rust, and a rule reference becomes a value — a built-in name, a component path, or a rule module — rather than an `import` statement. The reason is §13's: with rules as components the sandbox eventually leaves, and a config format that is not a program should not have needed one to be read.

**What replaces the mechanism is weaker, and is named rather than implied.** Two code paths can drift where one could not. Two things hold them together: both converge on the single function that constructs a `Config`, validates cards and severities and takes the hashes — so a divergence has to be introduced upstream of one place — and the §8.1 cache-key properties are asserted against both paths in matched pairs. A rule's `options` are among them, and are `config_hash` input on the JSON path because that is the path that knows them as data; on the TypeScript path they are written inside the config module and reach the key through `ruleset_hash`.

What has no successor is `lanekeep.config.ts`'s programmability once there is no JavaScript sandbox. Three shapes are plausible — JSON-only, a config-shaped component, or a minimal evaluator kept for configuration alone — and picking one is an open decision, not something the JSON path being sandbox-free settles.

---

## 10. Suppressions

```ts
// lanekeep-ignore-next-line local/no-numeric-sizes reason: legacy API requires exact 44
minWidth: 44,

// lanekeep-ignore-file local/no-primitive-components reason: generated fixture
```

- Rule IDs whitespace- or comma-separated, and **namespaced** — a bare id is rejected rather than silently matching nothing.
- `reason:` mandatory. A suppression is a decision to accept a violation, and the next person to read it cannot tell whether that decision still holds without one.
- Directive must be a standalone token, so prose mentioning it doesn't match.
- `--report-unused-suppressions` — hygiene. A suppression whose violation no longer exists documents a decision about code that has changed, and nothing else will ever say so. Reported as a **warning**: turning on a hygiene report must not fail a build that was passing. Off by default, because debt is worth surfacing on request rather than in everyone's inner loop.
- Optional `expires: YYYY-MM-DD` — surfaces as a violation past the date. Makes "temporary" suppressions actually temporary.

**A directive that does not work says so.** A missing `reason:`, a bare rule id, an unreadable `expires:`, a directive naming no rules — each is reported as a violation of `lanekeep/suppression` rather than skipped. The failure this guards against is a comment that looks like it silences something, does not, and never says so: the author moves on believing the violation is handled.

Directives are found by scanning for a standalone token rather than by walking comments in the parse tree. That is one pass over bytes already in memory and it works on a file that failed to parse. The cost is that a directive inside a string literal counts, which is a strange thing to write and shows up as a suppression that does nothing.

**An expired directive is reported but still silences.** Suddenly reporting everything it covered would turn a deadline into an avalanche on the day it passed. The expiry is a deadline in the ordinary sense: a directive dated the 31st still holds on the 31st.

### Suppressions and the cache

Two things follow from caching, and both are easy to get wrong:

- **Directives live in the cache entry**, because a reduce-phase violation is reported at a site in a file that may not have been reprocessed this run. Without them the warm path would drop the directive and report a violation the author had already accepted. The entry also records *which* directives fired: a warm run sees the survivors and not what was hidden, so without it every suppression in a cached file would suddenly look unused.
- **A file whose result depended on the date has the date in its cache key.** Two ways that happens: an expiring suppression in the file's bytes, or a rule that read `ctx.today` while checking it. Either way the entry lives for a day; every other file gets a dateless key and its entry survives indefinitely. Folding the date into *every* key would re-key the whole corpus daily for the sake of a handful of files, and leaving it out would serve yesterday's answer — an expiry that never expires, a date comparison frozen at whenever the cache was written.

  The expiry is visible in the bytes and so is known before the rules run; whether a rule reads `ctx.today` is not knowable until they have. So both keys are computed and the lookup tries the dated one first — a file that was date-dependent last run has its entry there, and if the date has moved that key simply misses. `ctx.today` is a property backed by a getter for exactly this reason: a plain value would be indistinguishable from an unread one, and every file would have to be treated as date-dependent.

The date is read once per run, from the host, in UTC. Once per run so two files checked a millisecond apart cannot disagree about what day it is; UTC because a deadline that moved with the reader's time zone would expire twice in some places and not at all in others. The sandbox still has no clock — a rule cannot observe the date, only a directive can be compared against it.

### 10.1 Fixes

A fix is a byte range and the text to put there — template-based replacement of a capture, not a general edit script. A rule that matched a node knows that node's extent, and replacing it covers almost every automatic fix worth having.

```ts
ctx.report(m.decl, {
  fix: { node: m.decl, text: 'let ' + name, safe: true },
})
```

The range comes from a **node**, never from offsets a rule computed. Offsets a rule works out itself are offsets it can get wrong, and a fix at the wrong offsets rewrites the wrong code.

**`safe` defaults to false.** A fix a rule did not mark is a *suggestion*: shown, never applied. The distinction is the author's to make and is not checkable, which is exactly why the default is the cautious one — the cautious mistake costs a manual edit, the other one silently rewrites someone's code.

`--fix` applies the safe ones and then **checks again**, because what a fix leaves behind is a different file and reporting the pre-fix violations would list things that are no longer there. The second pass is a cache miss for exactly the files that changed.

Two fixes touching the same bytes cannot both apply — the second would be editing text the first replaced. One is applied and the other skipped, chosen by start offset so the outcome does not depend on the order rules happened to run in, and the skipped count is always reported. A run that fixed three of five things and said it fixed everything would leave someone believing the file was clean.

A reduce-phase violation carries no fix: there is no parse tree in that phase, so no node to replace and no range to compute. Cross-file findings are fixed by hand.

---

## 11. Output

| Format | Purpose |
|---|---|
| `human` | Default. Path:line:col, rule id, message, indented remediation. Colors auto-off when not a TTY or `NO_COLOR`. |
| `json` | Machine-readable, stable schema, versioned. |
| `sarif` | Free GitHub code-scanning integration. |
| `agent` | Token-minimal, remediation-first, grouped by rule rather than by file. Includes rule cards for violated rules only. |

The `agent` format is a different document from the human one, not a terser rendering of it. Twelve violations of one rule are one thing to learn and one fix to apply; grouped by file they read as twelve problems. The card is stated once per rule rather than once per violation, which is the single largest saving available, and the remediation precedes the locations because the locations are only useful once the fix is known. A per-violation message that differs from the card's is kept — it says something the card does not.

`sarif` emits the required properties plus what GitHub actually reads. It describes each rule once and references it by index, and deliberately omits a rule-level `problem.severity`: severity is per violation here, since the same rule is an error in one config and a warning in another, and inventing a rule-level default to fill a field nothing requires is how a report starts lying.

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
lanekeep server [--protocol lsp|mcp]   # launched by editors/agent hosts
```

One-shot is the default and CI runs it unchanged. `--watch` is a foreground loop, not a background daemon. `server` is explicit and separate. The warm cache is what makes one-shot fast; process persistence is an optimization for editors, not a requirement for speed.

---

## 13. Security posture

Rules are executable code. The posture is therefore about **confinement**, not absence:

- **No ambient authority, in either engine.** Rule code reaches exactly the host functions in §6 and nothing else. For a TypeScript rule that is QuickJS with `fs`, `process`, `child_process`, network and dynamic import absent from the context rather than restricted within it. For a WebAssembly component (§6.9) it is structural: the component imports exactly one interface, `lanekeep:host/types`, so a clock, a filesystem and an entropy source were never bound and there is nothing to remove. A component importing anything else is refused at load rather than sandboxed at run time: the `wasm32-wasip1` build kept as a fixture imports **eleven** instances — the host interface plus ten WASI ones, among them `wasi:clocks/wall-clock`, `wasi:filesystem/types`, `wasi:filesystem/preopens` and the five `wasi:cli` interfaces — which is the ambient authority this check exists to keep out.
- **An empty import list is not an empty global object, and a JavaScript component needed a repair for the difference.** The paragraph above is about capability at the component boundary, and it is true of a `componentize-js` build: measured at the component's import list *and* at all 29 of its core-module imports, there is one import and it is the host interface. Rule code inside it nevertheless observed, on 2026-08-10:

  ```text
  Date.now=1786352655014 ; Math.random=0.48401551228016615 ;
  newDate=2026-08-10T09:04:15.014Z ; fetch=function ; setTimeout=function
  ```

  Byte-identical across two calls in one process and across two processes minutes apart, with `--disable all` passed and nothing to import a clock from. They are the JavaScript engine's *own* implementations, frozen by `wizer` into the memory image at the instant the component was built: `Date.now()` is the build time, `Math.random()` is a constant. Determinism survives — the component's bytes are a `ruleset_hash` input, so even the frozen timestamp is part of the cache key — but §6.6's claim does not, and present-and-frozen is worse than either absence or failure. A rule author calling `Date.now()` gets a stale build timestamp with nothing anywhere to say so.

  So the repair is a deletion, performed by `packages/lanekeep/runtime/host.js` before any rule module is evaluated: 37 names, including `Date`, `Math.random`, `performance`, `crypto`, `WeakRef`, `fetch`, the timers and `console`. Three things make it more than a list. It runs at the *runtime module's* top level and the generated entry imports that module first, because ES modules evaluate depth-first in source order — a rule imported before it would have its module scope evaluated while the clock was still reachable. Deleting is enough for the reason `sandbox.rs` gives: a rule may define its own `Math.random`, but whatever it writes is its own code and therefore deterministic, and the engine's original is unreachable once the only reference is gone. And a name that cannot be deleted **fails the build** rather than shipping, so a non-configurable global is a red build instead of a hole.

  Two tests hold it. `lanekeep-wasm/tests/js_globals.rs` holds the withheld list to what QuickJS withholds as an *equality*, so the two sandboxes cannot drift apart silently; and because that equality forbids pre-emptively withholding a name QuickJS has never heard of, a second test pins the 93 names the component's `globalThis` does have — which is what would notice a toolchain bump quietly adding a new clock. `Temporal` is the live case: SpiderMonkey ships it, this build does not have it, and without the pin a `componentize-js` bump enabling it would make `Temporal.Now.instant()` a clock with every test still green.

  **The lesson generalizes past this toolchain: absence at the import level is not absence at the language level.** A capability the component model can see is one a component cannot have; a capability the engine implements internally is one it has whatever the imports say.
- **No network.** Ever, in any mode, with no configuration that enables it.
- **Config load executes guest code, including WebAssembly.** A component answers `metadata`, `configure`, `has-check` and `has-reduce` when a config naming it is read, so `lanekeep rules` and `lanekeep explain` — which check no files — run guest code where they previously ran none. The confinement is the same one; the surface is that reading a config is enough to reach it. A `lanekeep.config.ts` has always had this property, and a `lanekeep.json` naming a component now has it too.
- **Filesystem confinement.** Reads happen only through `ctx.readFile`, only within the project root, with traversal rejected. Writes happen only under `--fix`, only to files a rule reported on, and only within the byte range of a node that rule matched.
- **Bounded execution.** A per-invocation timeout, a 15-second budget per phase of guest execution, and a per-runtime memory ceiling (§6.7). None disableable, and breaching any of them cancels the run rather than degrading to a partial result (§6.8). Note the per-phase scope: config load and the run each get the budget, so the process bound is a multiple of the configured number.
- **Determinism by construction.** No clock, no randomness (§6.6).
- Every built-in rule is reviewed by a maintainer.
- Supply chain: npm provenance / sigstore attestations, CI pinned to SHAs, `cargo-deny` + `cargo-audit` in CI, minimal dependency surface. This is a pre-commit-hook-adjacent tool and therefore a target.

**What this does not defend against.** A malicious rule in your own repository can still report misleading violations, read any project file, and consume its resource budget. lanekeep is not a boundary against someone who can already commit to the repository being checked — that person can modify the source and the CI configuration directly. The confinement exists to bound blast radius and to make third-party rule sets reviewable, not to make untrusted code safe to run unread.

---

## 14. One-way doors

Cheap now, breaking changes later. Lock all five before writing much code.

1. **Namespaced rule IDs from day one.** `lanekeep/<id>` for built-ins, `local/<id>` for project-authored, and any namespace a project declares in `namespaces:` for its own — `pera/<id>`. Bare IDs in v1 would break every config file, every suppression comment, and every consumer parsing JSON output when namespaces arrive. This is the expensive one.

    The set was originally closed to the two lanekeep defines, so that `lanekep/foo` was a typo rather than a valid ID matching nothing. Declaring keeps that property while letting a team group its own rules: an undeclared namespace fails at config load, naming the ones that exist. `lanekeep/` stays reserved, so a rule's origin is still readable from its ID alone.
2. **The host API is in the cache key** (§8.1), so adding a `ctx` function invalidates correctly rather than silently serving results computed without it. The component half is a *hash* of `wit/world.wit` rather than a version, because the version had to be bumped by hand and nothing detects a missed bump; QuickJS's half is still a hand-maintained number and leaves with the last JavaScript rule.
3. **Tracked effects from the start.** Retrofitting dependency tracking onto a cache that assumed purity means every existing entry is unsound. `deps` ships with the first cache.
4. **Nodes cross the boundary as handles, never as objects.** Materializing an AST for JavaScript is a decision that cannot be walked back once rules depend on the object shape.
5. **A clean internal `Rule` boundary.** Built-in rules are authored against the same host API as user rules — the strongest available evidence that the API is sufficient. A built-in needing to be reimplemented in another language does so behind the same ID and the same trait.

    **This one has been walked through twice, which is the only way a door gets tested.** The two Rust-checking built-ins were TypeScript modules and are WebAssembly components now (§4), and no config naming them changed: `lanekeep/no-unwrap` named the rule rather than its implementation before the port and still does. What the door bought is visible in what the port did *not* touch — rule IDs, suppression comments, output shape, the cache key's structure. Had the boundary been anything less than a trait both forms implement, that migration would have been a breaking change to every consumer instead of an internal one.

    The second walk is the stricter test, because it removes the confound. The first migration changed the language *and* the engine, so "no config changed" could have been a fact about rewriting a rule carefully. Four more built-ins have now moved to components with their sources **byte-for-byte unchanged** and their existing test files passing **unmodified** — same file, same language, different engine, and still nothing above the boundary noticed. A door you can walk through without editing the thing you carry is the one that was actually load-bearing.

---

## 15. Performance budget

Targets, gated in CI. Measured by `benches/corpus.rs` over a synthetic 2,000-file, 20-rule corpus.

| Scenario | Budget | Measured (dev machine) |
|---|---|---|
| Cold full run, ~2k files, ~20 rules | < 800 ms | ~1.1 s |
| Warm run, no changes | < 25 ms | ~64 ms |
| Warm run, 1 changed file, `--staged` | < 10 ms | ~51 ms |

These are noisy to within about 10% between runs on the same machine, so read them as
magnitudes rather than figures. The warm and `--staged` figures are higher than the ones
this table carried before the combined query landed, and the difference is the machine and
the session, not a regression — every number above was taken in one interleaved run against
the previous commit as a baseline, which measured cold ~1.76 s, warm ~67 ms and `--staged`
~61 ms on the same corpus minutes apart. Comparing a measurement to one taken on another
day is the mistake this table has already made once.

**Nothing meets its budget yet, and the budgets stand.** They are targets to aim at, not release gates — a number chosen before anything existed does not get to decide whether the thing that exists is worth shipping. What they are for is direction: they say which way is better, and the gap between them and the measurements says how much room is left.

Measuring them is not grounds for moving them. The levers below are the answer, in that order, and relaxing a budget remains the last resort — the point of a target you have not hit is that it keeps pointing.

**The earlier numbers in this table were stale, and understated the gap by about half.** They
were taken before Python, Go and Rust support existed; three more grammars means more rules
compiling more queries, and cold went from ~1.5 s to ~3.3 s without anyone noticing. Worth
holding on to generally: a "measured" column is a claim with a date on it, and this one had
drifted for four releases while being read as current.

What measurement has bought so far:

- **A warm run built a sandbox per worker and evaluated every rule module into it**, then executed no JavaScript because every file was a cache hit. Making the sandbox lazy — built on the first match that actually needs one — took a warm run from ~263 ms to ~56 ms. §7.3's claim that a warm run runs no JavaScript was true; it was paying to be *able* to.
- **A subset run discarded the cache entries for every file it did not look at.** `--staged` left the next full run cold, which is the opposite of what an incremental entry point is for. A run now prunes only when it saw the whole corpus.

- **Compiling queries cost more than the entire warm run.** `Engine::prepare` was measured at ~88 ms against a ~55 ms warm run: a tree-sitter query takes a couple of milliseconds to compile, a rule compiles one per language it declares, and twenty rules over two languages is forty compilations before a single file is read. Compiling them in parallel took warm from ~102 ms to ~65 ms and `--staged` from ~75 ms to ~30 ms.

  **None of the levers listed below named this**, which is the more useful lesson: the analysis reached for the parts of the design that were interesting — the sandbox, the cache format, the staleness check — and missed a plain constant cost sitting in front of them. The first profile of a warm run showed setup outweighing work, and that was not a hypothesis anyone had written down.

- **Every rule walked the tree itself.** The parse was shared; the traversal was not, so
  twenty rules meant twenty `QueryCursor` passes over one tree — 40,000 walks across the
  corpus. tree-sitter evaluates many patterns in a single traversal, which is what a
  `highlights.scm` is, and doing it that way took cold from ~1.76 s to ~1.06 s.

  Found by arithmetic on the profile rather than by reading it. Cold time was linear in the
  rule count at ~380 ms per rule, and four rules that matched *nothing* still cost 250 ms
  each — a cost proportional to rules rather than to matches is a cost per traversal. A
  micro-benchmark then put 20 separate cursors at 13.5× one combined query at identical
  capture counts, and reusing a single cursor across the separate queries recovered only
  1.7× of that, which said the traversal was the cost rather than the allocation.

  The marginal cost per rule is now ~133 ms and *falls* as rules are added (239 → 143 → 85
  over 5, 10 and 20 rules), which is the shape a shared traversal predicts: the first rule
  pays for the walk and the rest add only their own patterns.

  `--profile` deliberately keeps the old path. The split it reports — query time against
  handler time — measures one rule in isolation, and a shared traversal has no honest way
  to divide itself among the rules sharing it. So the profile now reports more query time
  than the run pays, which is a sharper version of the caveat below: **the profile accounts
  for what rules do, not for what a run does.** Two paths through the hot path is somewhere
  divergence could hide, so a test asserts both report the same violations.

  Both halves of it had to be made lazy before it was a win everywhere. Compiling the
  combined query eagerly cost a warm run 26 ms, and merely *assembling* its source cost
  another 4 ms — both paid in full by a run where every file is a cache hit and no query
  ever runs. Warm has the tightest budget of the three scenarios; it does not subsidize
  cold.

- **A file was parsed once per rule, not once.** §2 and §7 both say the queries run across a single shared parse; `run_rule` built its own `tree_sitter::Parser` instead, so a file admitted by twenty rules was parsed twenty times. Cold went from ~3.3 s to ~2.1 s when the parse moved up to the file.

  It hid because `--profile` never saw it. The per-rule clock starts *after* the parse, so the re-parse was not attributed to query time or handler time — it was not in the table at all. The per-rule numbers summed to well under the run's wall clock and nothing pointed at the difference, which is the failure mode worth naming: **the profile accounts for what rules do, not for what a run does**, and a cost outside every rule is invisible to it rather than misfiled.

  What found it was arithmetic on a number the profile *did* report: 0.23 ms to run a one-pattern query over a 1.5 KB file is an order of magnitude too slow, and chasing that gap led to the parse.

  `Tree::clone` is `ts_tree_copy`, a refcounted copy, so every rule still gets an owned tree for its arena at almost no cost. A test asserts the engine constructs exactly one parser outside its tests, because parsing per rule produces identical output and simply runs N times slower — there is nothing to notice.

  **An earlier version of this passage claimed the profile had attributed the re-parse to query time.** It never did; the timer starts after the parse in both the old code and the new. The correction matters because the false version made the instrument sound more trustworthy than it is — it says nothing about time spent between rules, and a reader planning the next optimization needs to know that.

  It is deliberately *not* lazy. Compiling on first use would take warm setup to nearly zero and would cost what the §16 comment describes: a broken query is reported at preparation, naming its rule, rather than staying silent until some file happens to need it. Parallelism buys most of the win without trading that away.

**Why "1 changed file, full discovery" has no budget.** Finding which file changed means reading and hashing all of them, so that scenario is the row above plus one file's work and can never beat it. §15's 10 ms describes the pre-commit workflow, and the pre-commit workflow is `--staged` — which is where the budget now sits.

The cold budget is looser than a pure-Rust engine would allow, and that difference is the honest price of programmable rules: handler invocation and QuickJS interpretation cost real time on every match. The query gate (§7.2) is what keeps that cost bounded to matches rather than nodes.

If the cold budget proves unreachable, the levers in order are: better gate usage in built-in rules, bytecode caching across runs, then a faster engine behind the §5.1 trait.

And one thing it has ruled out, which is worth as much:

- **Sharing the file's source across rules bought nothing.** `HostContext::new` copies the
  file's text into every rule's arena, so twenty rules meant twenty copies — about 72 MB of
  copying across the corpus. Replacing the `String` with an `Arc<str>` so every rule shares
  one allocation produced byte-identical output and **no measurable improvement in a
  parallel run**: 1320 ms before, 1353 ms after, which is noise. Single-threaded it was worth
  about 4%, and that is the whole of it.

  The hypothesis had a real signal behind it, which is why it was worth testing: padding the
  corpus with comments — more bytes, same nodes, same matches — did make the per-rule cost
  grow. But it grew *sub*-linearly, 2.1× for 9.3× the bytes, and a two-point model built on
  that predicted the third point 43% low. A cost that will not fit a line in the variable you
  think drives it is a cost driven by something else.

  Not shipped, because it changes `HostContext::new` and `NodeArena::new` in the public API
  for a gain no user would ever observe. Worth writing down so the next person reads the
  measurement instead of repeating it — and worth noting that this was the fourth of five
  hypotheses about where cold time goes to be wrong. In this area the profile and the
  intuition have both been poor guides, and only arithmetic on measured numbers has found
  anything: the cache I/O that turned out to be 0.37 ms, the "expensive queries" that turned
  out to be a re-parse, the misattribution that never happened, and now this.

The remaining warm cost is reading and hashing every file to discover what changed, plus loading and rewriting a whole-corpus cache file. Beating it needs either a cheaper staleness check than content hashing, or a cache format that can be written in part.

**A component must be compiled once per project, not once per config load.** Config load asks each component what it is (§6.9), which means compiling it, and compiling one is tens of milliseconds — so a config load with nowhere to cache the result pays that on every invocation, and pays it again at prepare time through the engine's own loader. Measured on the release binary before this was closed: `lanekeep rules` on a one-component project took 67.6 ms against 8.6 ms for a TypeScript-only one, and 124.3 ms with two components — a command that checks no files, exceeding the 25 ms warm budget on config load alone. It compounds because config load is not once per session: it runs per LSP request, per MCP tool call and per `--watch` iteration, and `lanekeep init` scaffolds a component-backed rule into every Rust project.

The fix is that config load and the engine share one artifact cache, `.lanekeep/components`, so the first run in a project compiles once and every later run maps what it wrote. After: 8.6 ms and 9.3 ms for one and two components, against 10.8 ms for TypeScript only — components no longer measurably cost anything at config load. The first run in a project still pays the compile, ~70 ms per component. `lanekeep_config::load` keeps the uncached behavior, because a caller that has not named a project root has not asked for a directory to be written.

**Every number in the two paragraphs above was measured against components of about 26 KB, and a JavaScript one is 12.4 MiB. It does not generalize, and the gap is three orders of magnitude.** Measured 2026-08-11 on the release binary, Apple M3 Max, over a one-file project — so essentially all of this is config load and prepare rather than checking:

| Config | Cold | Warm |
|---|---|---|
| one TypeScript **module** rule | 32 ms | ~30 ms |
| one rule of the shared JavaScript component | 6,115 ms | ~213 ms |
| two rules of it | 6,680 ms | ~747 ms |
| three | 7,451 ms | ~1,492 ms |
| **four — the migrated set** | **8,312 ms** | **~2,398 ms** |

So "compiling one is tens of milliseconds" is ~6 s for this artifact, "~70 ms per component" is ~6 s, and "components no longer measurably cost anything at config load" is 213 ms for one rule against a 25 ms warm budget. The precompiled `.cwasm` is 34 MB.

**The warm column grows faster than the rule count, and that is a defect rather than a property.** The increments are +534 ms, +745 ms, +906 ms: `lanekeep-config` loads once per rule *reference*, so four rules of one component deserialize the same 34 MB artifact repeatedly and instantiate it more than once, where the whole point of sharing the component is to pay for the engine once. The fix is a memo keyed on the content identity `Loaded` already carries — the same identity `lanekeep-wasm` keys instance sharing on (§6.9) — rather than anything structural. It is not done, and until it is, **naming a fifth rule of that component costs about another second of warm time**.

None of this is on the path of a project that uses no components, and none of it changes a result. It is the cost of the form, and §15.1 has the other half — what a component costs per `check` invocation once it is loaded.

**The report is absolute; the gate is relative.** Absolute numbers cannot be gated on a hosted runner: this suite's first CI run measured a cold pass at 10.9 s against 1.5 s on a developer machine, seven times slower, on hardware that varies between runs. Any absolute threshold loose enough not to flake there is too loose to catch anything.

So the two jobs are separated. The report prints every scenario against its budget, exactly, on every run — that is what shows a 20% regression to someone reading the log, and it is why the budgets stay in the table even though none is met. The gate asserts only machine-independent ratios, chosen to be the claims the design rests on:

| Gate | Threshold | Developer machine | Hosted runner |
|---|---|---|---|
| warm run ÷ cold run | ≤ 25% | ~4% | ~1% |
| selected 1-file run ÷ warm run | ≤ 1.5 | ~0.6 | ~0.9 |
| cold run, absolute ceiling | < 60 s | 1.5 s | 10.9 s |

The first is the cache's entire purpose, and it is the check that caught a warm run starting a QuickJS engine per worker. The second says a file selection must not cost more than no selection. The ceiling is not a budget — it is 75× the budget — and exists so an infinite loop or a quadratic blowup fails the job instead of running until the runner gives up.

Instrumentation behind `--profile` only, reporting per-rule time split between query matching and handler execution — the split that tells an author whether their query or their code is the problem — plus the match count, which is the number §7.2's gate exists to keep small.

It goes to stderr, so `--profile --format json` still pipes a clean document, and it is sorted by total cost with the rule id breaking ties, so the table does not reorder between runs for reasons nobody can see. A rule that matched nothing still appears: its query ran on every file, and the rule whose query is expensive *and* matches nothing is the worst case there is.

Off by default, because measuring costs a clock read per handler invocation and the warm path is the one place that matters most.

### 15.1 What a host-API crossing costs, in each engine

§4's invariant is that JavaScript executes proportional to matches and that any change increasing boundary crossings per file needs a benchmark rather than an argument. Putting a rule in a WebAssembly component was expected to leave the number of crossings alone and change what one costs, because the canonical ABI copies strings and lists through linear memory where `rquickjs` handed QuickJS a value. That was the one unmeasured quantity that could have inverted the case for components, and the decision record made measuring it a condition of acceptance. `benches/crossings.rs` is the measurement.

**The subject is `lanekeep/no-unwrap`, run as each.** It exists in three forms — the TypeScript module that was the shipped rule until `1fb5d06`, the Rust component that replaced it, and that same TypeScript source compiled into a component by `componentize-js` — and it is the heavier of the two ported built-ins by a wide margin. The two corpora are byte-identical except for a six-character method name: `unwrap`, which sends the rule up the ancestor chain looking for a `#[test]`, against `mapmap`, which it drops after one call. Everything that is not a crossing — reading, hashing, parsing, query matching, the invocation itself — is identical on both sides and subtracts out.

Measured 2026-08-10, Apple M3 Max (14 cores), macOS 26.5.2, rustc 1.95.0, wasmtime 47.0.3, rquickjs 0.12, tree-sitter 0.26. **Both arms in one session on one machine**, single-threaded so that a figure printed as nanoseconds per call is nanoseconds per call and not a throughput divided by the width of the machine.

| Arm | Cold corpus | Hot corpus | Difference | Host calls | Per call |
|---|---|---|---|---|---|
| TypeScript, QuickJS | 115 ms | 294 ms | 179 ms | 593,280 | ~302 ns |
| component, wasmtime | 78 ms | 216 ms | 138 ms | 414,720 | ~332 ns |

**"Host calls" is the *marginal* count, `hot − cold`, which is what "Difference" is divided by.** The cold corpus makes 3,840 calls in either arm — one per subject, before the method name sends the rule up the ancestor chain — so the marginal counts are 3,840 below the hot totals of 597,120 and 418,560 that the prose below cites and the bench prints in its own block. Both quantities are right and they are not the same one: a per-call figure has to divide a marginal time by a marginal count, or it is a rate between two different populations. This has been "corrected" once, in the direction of making the table agree with the prose and stop agreeing with the measurement. Run `cargo bench -p lanekeep-engine --bench crossings` before changing it; its `calls` column is this one.

**A host call costs about 1.1× through a component.** More than a dozen runs on a settled machine gave 1.06 to 1.12 — QuickJS 298–308 ns, component 322–335 ns — which is the whole of the spread. (A run started immediately after a rebuild reads outside it, up to 1.15; the QuickJS side is the steady one and the component side is where the noise lives.) The performance argument is not inverted, and it is not close to inverted.

**The denominators are different on purpose, and that is half the result.** The port hoisted `line(ancestor)` and `column(ancestor)` out of the sibling loop, so the component saves two calls per sibling it scans; against that it pays one more per match, because `filePath` is a property under QuickJS and a method on `check-context`. Over this corpus the component makes 30% fewer host calls than the rule it replaced. A benchmark that divided both times by one crossing count would have reported the two rules' relative efficiency and called it the cost of the boundary — and would have concluded, wrongly, that a component crossing is *cheaper*.

So the end-to-end direction and the per-call direction disagree, and both are true: the component arm is **faster** on this corpus, by 27% on the hot one, while each of its host calls is dearer.

Three things this does not say.

- **It is not the cost of the boundary alone, and the split beneath it is indicative rather than measured.** Between two crossings a rule also executes, and that execution is interpreted bytecode in one arm and compiled code in the other. The bench separates the arena work — the same call sequence replayed against the same `NodeArena` with no engine in the way — and what remains is the crossing *plus* the in-guest execution, which nothing here splits further. Over seven runs the arena figures are steady, 200.8–203.1 ns and 233.2–237.2 ns; the residues are 100.6–106.8 ns and 86.6–101.2 ns. **Those two ranges nearly touch.** The residue is a difference of two separately timed quantities, so it carries the whole of the engine measurement's absolute noise on a third of the magnitude — 15% against the 4% of the figure it came from. The direction held in every run observed, and the gap is not established at the precision the numbers are printed to. So: about two-thirds of a QuickJS call and about three-quarters of a component call is shared arena work, and the honest reading of a residue that favors the component is not that the canonical ABI is cheaper than `rquickjs` — it is that whatever the ABI costs extra, compiled guest code appears to more than pay back. **The headline ratio is the result; this split is the scale it sits on.**
- **They are two call mixes, not one.** Both are dominated by scalar and short-string returns plus one `named-children` per match returning a list as long as the file's top-level items — but they are not the same mix, and for the same reason the denominators differ: the hoist above removes two cheap position lookups per sibling from the component's sequence, so a larger share of what is left is string-returning. That asymmetry, and the violation construction and cache write that sit inside the timed region on the hot corpus only, both bias **against** the component. 1.1× is a ceiling rather than a midpoint. A rule that moves large lists will meet the copy cost more directly than this one does.
- **The call counts are replayed, not counted by the engine.** Neither engine exposes a host-call counter and neither grew one for this: an increment on the trust boundary's hot path is a poor trade for a benchmark. The bench instead follows the rule's decision procedure statement for statement over the same trees, counting what it would have called. Two things hold that honest, and the weaker one is the one that looks convincing. The bench's assertion holds *each arm* to *its own branch* of the replay and then the branches to each other — per arm, because holding both engines to one branch leaves the other validated by nothing, which is what an earlier version did and what breaking the TypeScript branch alone now demonstrates. But the assertion cannot see a call omitted on a path all three take. What pins the numbers is arithmetic: the corpus is regular enough that both denominators are derivable in closed form — 40 files of 96 functions, an ancestor chain of four, a call in the *k*-th function scanning *k* siblings, giving `13 + 3k` calls per subject under QuickJS and `14 + 2k` under the component — and the sums are exactly the 597,120 and 418,560 the bench prints. Two independent derivations of one number, not one transcription trusted twice. **Changing the corpus shape or the rule means redoing that sum**; the assertion alone would not notice.

**No budget moved.** This measures one thing and writes it down; the table above is re-baselined once the remaining authoring paths land.

#### The third arm: the same TypeScript, inside a component

The two arms above differ in two things at once — the boundary crossed and the language written — so a ratio between them cannot say which of the two moved. The third arm holds the rule still and changes only the engine underneath it: the *same* `no-unwrap.ts` the QuickJS arm runs, compiled by `componentize-js` into a component with the flags the shipped built-ins are built with. Its artifact is 13 MB and is not committed, because every crate here is published and crates.io refuses a package over 10 MiB; `just bench-js-component` builds it under `target/`, and the bench prints two arms and names that recipe when it is absent.

Measured 2026-08-11 on the same machine — Apple M3 Max (14 cores), macOS 26.5.2, rustc 1.95.0, wasmtime 47.0.3, rquickjs 0.12, jco 1.27.0, componentize-js 0.22.0 — **all three arms in one session**, single-threaded.

| Arm | Cold corpus | Hot corpus | Difference | Host calls | Per call | Against QuickJS |
|---|---|---|---|---|---|---|
| TypeScript, QuickJS | 126 ms | 318 ms | 192 ms | 593,280 | ~324 ns | 1× |
| Rust component, wasmtime | 82 ms | 229 ms | 147 ms | 414,720 | ~355 ns | 1.10× |
| TypeScript component, wasmtime | 621 ms | 22.1 s | 21.5 s | 597,120 | ~35,900 ns | **111×** |

"Host calls" is the marginal count here too, for the reason the note above gives — and **597,120 now appears in this document as two different quantities**, which is worth flagging rather than leaving for a reader to trip over. It is QuickJS's *hot total* in the paragraph above and the JavaScript arm's *marginal* count in this table. Both are right and the coincidence is arithmetic: the JavaScript arm makes one more call per engaged match than QuickJS does, and there are exactly as many engaged matches as there are cold-corpus calls. The three hot totals are 597,120, 418,560 and 600,960.

**A host call costs about 110× through a JavaScript component.** Three runs in that session gave 109.3, 111.0 and 112.6 — 34.3 µs to 35.9 µs against QuickJS's 313–324 ns. End to end on this corpus, which is built to make crossings as visible as possible and is not a real one, the hot run is 21.5 s against 313 ms.

That is stated first and without softening because it is the direction the decision record was afraid of. The condition attached to accepting components was that the per-crossing cost be measured before the self-check rules moved, precisely so a number like this could arrive before it was expensive to act on. For a Rust component it came back at 1.1× and the case held. For a JavaScript one it is two orders of magnitude, and the case for that form has to rest on something other than speed.

Four things pin it, and the fourth is the one that keeps it from being read as a measurement of the boundary.

- **It is per crossing, not an artifact of the corpus.** Halving the functions per file to 48 cuts the marginal crossings by four — 593,280 to 158,400 — and the per-call figure does not move: 34.3 µs against 35.0 µs on the full corpus. A cost that scales with the crossing count is a cost per crossing.
- **It is the sturdiest of the three figures, which is not a compliment.** One run in the session was contaminated by other work on the machine and read QuickJS at 1,825 ns and the Rust component at 3,717 ns — six and ten times their settled values — while the JavaScript arm read 34,959 ns, unchanged. A cost that large is insensitive to everything else happening; the ratio's noise lives entirely in its denominator.
- **It is not the canonical ABI.** The Rust component pays the same ABI on the same host code and comes out at 1.10×. The bench's `arena` column — the identical call sequence replayed against the same `NodeArena` with no engine in the way — is about two-thirds of the QuickJS figure and **0.6%** of the JavaScript one (≈209 ns of ≈35,000). Practically all of the difference is engine.
- **What that engine is doing is interpreting, and it cannot do otherwise.** A JavaScript engine compiled to WebAssembly has no JIT to fall back on: there is no way to emit and enter machine code from inside the sandbox, so SpiderMonkey runs the rule in its interpreter — inside a guest that is itself compiled code being called through the component model. Every host call additionally traverses `host.js`'s `ctx` shim and `componentize-js`'s generated bindings. §5.1 accepted interpretation as affordable "precisely because of the query gate", and that argument still holds in shape; what changed is the constant, by two orders.

#### The larger cost, which the measurement above subtracts away

Everything above is *per crossing*, and it is a difference between two corpora with the same match count — so whatever a `check` invocation costs **before** the rule crosses anything cancels out of it exactly. That is the right thing to remove when comparing QuickJS against a Rust component, because there it is small. For a JavaScript component it is the bigger number, and it is the one that decides the outcome for a rule that does not cross much — which is all four of the rules that actually migrated.

The bench measures it by running the cold corpus at two sizes, 10 files and 40, and taking the **slope**. Every fixed cost of a run — config load, discovery, engine and component instantiation, the cache write — falls out of a slope, and on the cold corpus one match is exactly one `check` invocation and exactly one crossing. Same session, same machine:

| Arm | Per file | Per match | Intercept | Per invocation, against QuickJS |
|---|---|---|---|---|
| TypeScript, QuickJS | 2.89 ms | 30.1 µs | +8.2 ms | — |
| Rust component | 2.03 ms | 21.1 µs | +2.0 ms | **−9 µs** |
| TypeScript component | 15.5 ms | 161 µs | **−31 ms** | **+98 µs** |

The per-match column carries the shared per-file work — reading, hashing, parsing, query matching — which is identical over identical bytes and cancels only when two arms are differenced; the last column is that difference with the one crossing taken out. Three runs gave +107.4, +100.3 and +97.7 µs, and an independent reviewer's own extraction from a separate pair of corpus sizes gave ≈107 µs.

**The intercept is the part to read before believing the slope.** It is everything that does not scale with the corpus, which is exactly where instantiating a 13 MB component would appear. For the JavaScript arm it is *negative* — −29 to −31 ms across three runs — so there is no fixed cost hiding in this figure at all. The ~100 µs is charged per invocation, not once per run.

Two consequences, and the second is the one worth planning against.

A Rust component's invocation is about 9 µs **cheaper** than a QuickJS one, which is a small result in the opposite direction and is why the Rust arm's cold column is lower than QuickJS's throughout. And the added cost of the JavaScript form is roughly `matches × 130 µs` before a rule crosses anything at all: about 100 µs of invocation plus the one crossing every match makes. Ten thousand matches is **1.3 seconds**, against §15's 800 ms cold budget for a whole 2,000-file run.

**What this says about the four migrated built-ins, and it is not what the per-crossing figure says.** `no-unwrap` was chosen as the subject for being the heaviest crosser that ships — seven distinct host functions where `no-glob-import` calls two, and about 155 crossings per engaged match on this corpus. **The four rules that migrated are the opposite.** They are low-crossing rules, so driving their crossings to *zero* would still leave ~100 µs per invocation, and the 110× headline is nearly irrelevant to them.

So the lever is **fewer matches, not fewer crossings**: the query gate of §7.2, a `fileContains` gate that keeps a file from being parsed at all, and a query that binds the site the rule actually cares about instead of a broad shape it then filters in JavaScript. Reducing crossings per match is the right lever for a *heavy* rule such as `no-unwrap` — the Rust port's hoist of `line`/`column` out of a loop took 30% of its calls away, and that lever is available to a TypeScript rule unchanged — and it is the wrong one here.

The four TypeScript built-ins now sharing one component do not run faster than they did as modules, and nothing in this document should be read as claiming they do. The case for compiling them is the authoring path and eventually one engine rather than two, not throughput. §15's budget table has **not** been re-baselined against them, and §15's config-load figures are the other half of the bill.

**A TypeScript rule that is not compiled is unaffected.** This is the cost of the component form, not of authoring in TypeScript: a rule loaded as a module still runs in QuickJS at the first row's price. The coexistence window (§6) is what makes that a choice rather than a migration everyone pays for.

---

## 16. Milestones

**Every milestone below is delivered.** lanekeep checks TypeScript, TSX, JavaScript, Python, Go and Rust; ships ten built-in rules, six of them as WebAssembly components; and is distributed through npm, PyPI, crates.io, Homebrew and as a Go module, one build feeding all five.

Two things named here are still outstanding, and each is stated where it belongs rather than only here: the §15 performance budgets are targets that are not met, and M5's authoring path compiles the rules that ship with lanekeep but not a project's own.

**M0 — walking skeleton. Done.** Workspace, config loading, discovery, tree-sitter TS/TSX, query compilation, the embedded engine with the §6 host API, human + json reporters, `RuleTester`. Acceptance: the built-in rules and a representative set of project-authored rules run end-to-end against the fixture corpus, with snapshot-verified output — every reporter is snapshotted in `lanekeep-report/tests/snapshots.rs`, and the built-in rules are driven through the binary over real corpora in `lanekeep-cli/tests/`.

**M1 — speed. Done, with the budgets outstanding.** Cache with dependency tracking, `--since` / `--staged`, rayon parallelism, `benches/` in CI with regression gates. Acceptance: the suite exists, runs in CI, and gates on regression — all of which it does. The §15 budgets are targets rather than an acceptance criterion; none is met yet, and §15 says by how much and what the levers are.

**M2 — completeness. Done.** Full host API surface, SARIF + agent reporters, `explain`, fixes (template-based replacement of a capture, marked machine-applicable vs suggestion), unused-suppression reporting.

**M3 — loops. Done.** `--watch`, and `server` speaking both LSP and MCP.

`--watch` is a foreground loop, and the trap it exists to avoid is that lanekeep writes its cache into `.lanekeep/` *inside the root it watches*. A watcher reacting to every event under the root sees its own write, re-checks, writes again, and never stops — at full CPU, while looking like it is working. The filter that prevents it is the part with tests.

`server` is hand-written JSON-RPC over stdio, because `deny.toml` denies `tokio` and that rules out every async LSP crate. The constraint turned out to be the right shape: a server that reads a message, answers it, and reads the next has nothing to schedule. It also made the MCP half cheaper than expected — both protocols are JSON-RPC 2.0 over stdio, differing only in framing and method set, so `lanekeep-server::jsonrpc` carries both framings and MCP added a dispatch table rather than a transport.

MCP exposes three tools, one per thing the CLI already does: `lanekeep_check`, `lanekeep_rules`, `lanekeep_explain`. They return the `agent` reporter's text unchanged, because that format exists for exactly this consumer. The distinction MCP draws between a failing *tool* and a failing *call* is load-bearing and is tested: a rule that throws comes back as `isError: true` with the message as content, because it is a result the model should act on, while a malformed argument is a JSON-RPC error for the host.

Diagnostics publish on open and on save, not per keystroke: a check reads from disk, and the buffer an editor holds mid-edit is not there yet. Publishing against stale bytes puts squiggles under the wrong characters, which is worse than a save-length delay on a warm cache.

**M4 — a second language, then a third and a fourth. Done.** Python was the cheapest proof that the `Language` trait abstraction actually holds, and it held: `lanekeep-core` is untouched. `lanekeep-lang-python` implements `Language` and `BindingResolver` and nothing above it needed to know.

Two things did change, and both are the abstraction working rather than leaking. `lanekeep-lang` gained binding kinds — `assignment`, `loop`, `context-manager`, `comprehension` — because Python binds names in ways JavaScript has no word for, and answering `ctx.bindingKind` with `var` for all three would have been untrue. And `lanekeep-languages` was added as the composition root: the CLI and the testkit both need to know which languages exist, and no crate below them can hold that answer without inverting the dependency the trait exists to create.

Go followed, and is the stronger evidence: the second language could have been accommodated by a trait shaped around the first two, whereas the third arrived after the shape was fixed. `lanekeep-core` was untouched again. `lanekeep-lang` gained three more binding kinds — `type`, `receiver`, `type-param` — for the same reason Python's were added: a struct is not a `class` and a method receiver is not quite a `param`, and answering `ctx.bindingKind` with the nearest existing kind would be untrue. Go's scoping needed genuinely new modeling rather than a variation on Python's, since its package block is order-independent and several of its statements are scopes without being blocks.

**Distribution followed each language.** A checker that reads Python should be installable by a Python project on its own terms, and the same for Go, rather than telling either team to install Node. PyPI takes one platform wheel per target and needs no launcher, because a wheel names its platform in its own filename. Go needed one, because Go's tooling installs and pins only things written in Go — and it needed no publish step at all, because a Go module version *is* a git tag. [`docs/releasing.md`](releasing.md) has both.

Building the PyPI lane surfaced a bug none of the other channels would have: the Linux binaries had inherited a glibc 2.39 floor from the runner image, so they did not start on Ubuntu 22.04, Debian 12 or RHEL 9. A wheel is the first artifact that must *name* its floor, which is why it surfaced there. The floor is now pinned at 2.17 in the build and asserted against the binary before anything is tagged.

Rust is the fourth, and the one whose *patterns* do the most work. The other three bind a name by writing it; Rust binds several at once through destructuring, and the shape doing it also names constructors that are matched rather than bound — `let Some(v) = opt` binds `v` and not `Some`. Getting that wrong is worse than resolving nothing, because every constructor in a file starts resolving to a local and rules that ask whether a name is an import begin answering no. Two more binding kinds were added, `module` and `trait`, on the same test as before: the nearest existing kind would have been untrue.

What Python does *not* share with JavaScript is the interesting part. It has no block scope, so resolving a name means walking a scope's whole body rather than its direct children — the JavaScript resolver can look at direct children only because a block *is* a scope there. And a class body is opaque to functions nested inside it, so `def m(self): return LIMIT` does not see a class-level `LIMIT`. Neither rule could be expressed by reusing the JavaScript resolver, which is the evidence that mattered: the trait is the boundary, not a shared implementation.

**M5 — a second authoring path for the first language. Done, with one thing deferred.** A TypeScript rule can be compiled ahead of time into a WebAssembly component (§5.2) instead of being loaded into QuickJS, and four of the built-ins ship that way from sources that did not change by a byte. The acceptance test was picked to be one nobody could argue with: those four rules' sources are frozen and asserted with digests, and the four test files that covered them as modules pass **unmodified** against them as a component. Output fidelity was checked the only way that means anything — two release binaries over one unchanging corpus, same md5, same 42 violations, same per-rule split — rather than one binary over two trees, which measures the tree.

Four things this bought, and one it cost.

It closed the last structural gap in §14's fifth door: a built-in reimplemented in another language was already known to be invisible to a config, and now a built-in *not* reimplemented — the same file, compiled differently — is invisible too. It gave the component path a second source language, which is what turns §6.9's rule index from a Rust convenience into the thing four rules actually share. It put lanekeep's resolution rules in two languages with a parity test between them (§5.3), which is the first time that boundary has had a second implementation to disagree with. And it produced the diagnostic a compiled rule needs and a loaded one does not: a sidecar source map, so a rule that throws is reported at its own line rather than at a position in a bundle nobody can open.

The cost is §15.1's, stated there and not softened here: a host call from a JavaScript component is about 110× a QuickJS one. The four rules are not faster for having moved.

**Deferred: compiling a project's own TypeScript rules on demand.** Everything above compiles rules that ship *with* lanekeep, at lanekeep's build time, by a maintainer running a recipe. A project's own rules still load into QuickJS, and there is no path that compiles them. That is deliberate rather than unfinished: on-demand compilation needs Node on a user's machine, or a bundler and a JavaScript engine inside the binary, and neither is a decision to make before something real needs it. The sixteen rules this repository checks *itself* with are the first such consumer, and they are what will decide the shape.
