/**
 * Type definitions for authoring lanekeep rules.
 *
 * **Generated from `crates/lanekeep-wasm/wit/world.wit` by `crates/lanekeep-types-gen`.** Do not
 * edit by hand — run `just generate-index-dts` and commit the result.
 *
 * These describe the host API a rule reaches inside lanekeep's sandbox. Nothing here runs in
 * Node: `defineRule` and `defineConfig` are identity functions whose only job is to give the
 * compiler something to check against, and `RuleContext` is provided by lanekeep at run time.
 * The world is the single source of truth for every member the renderer emits straight from it.
 * Three members deviate from the world on purpose, and all three are QuickJS-shaped: `today` is
 * omitted from `RuleContext` because QuickJS exposes it as a conditional property rather than a
 * callable, a shape this renderer cannot state honestly from the world; `facts` is added to
 * `RuleContext` because QuickJS hands a per-file rule `facts` that the world declares only on
 * `reduce-context`; and `types` is added to `RuleContext` because `ctx.types` — the bounded
 * type oracle — is QuickJS-only and has no presence in `world.wit` at all: a component rule
 * cannot declare `requires`, so there is nothing for the world to say about it. Nothing else is
 * added or omitted by hand.
 */

/**
 * A node in the parse tree.
 *
 * Deliberately opaque. Nodes cross into the sandbox as integer handles rather than objects,
 * and the reason this is a branded type rather than `number` is that **the root node's handle
 * is `0`** — written as a plain number, `if (!node)` looks like a null check and silently
 * discards the root. Compare against `undefined` explicitly.
 */
export type Node = number & { readonly __lanekeepNode: unique symbol }

/** How a name was introduced, as `ctx.bindingKind` reports it. */
export type BindingKind =
  | 'import'
  | 'const'
  | 'let'
  | 'var'
  | 'param'
  | 'function'
  | 'class'
  | 'catch-param'
  | 'assignment'
  | 'loop'
  | 'context-manager'
  | 'comprehension'
  | 'type'
  | 'receiver'
  | 'type-param'
  | 'module'
  | 'trait'

/** A language lanekeep can parse. */
export type LanguageId = 'typescript' | 'tsx' | 'javascript' | 'python' | 'go' | 'rust'

/** How serious a violation is. */
export type Severity = 'error' | 'warn' | 'off'

/**
 * The captures of one query match, keyed by capture name without the `@`.
 *
 * A capture that did not participate in the match is absent, which is why the values are
 * optional — an alternation like `[(a) (b)] @x` binds `@x` either way, but two separate
 * patterns capturing different names do not.
 */
export type Match = Record<string, Node | undefined>

/**
 * What a rule tells whoever has to act on the violation — increasingly an agent.
 *
 * Not documentation, and not optional. `remediation` is the field worth the effort: it should
 * say what to do, not restate the problem.
 */
export interface RuleCard {
  /** What is wrong, in a few words. */
  message: string
  /** What to do about it. */
  remediation: string
  /** One example each way. */
  examples: {
    bad: string
    good: string
  }
}

/**
 * Cheap rejections applied before a file is read or parsed.
 *
 * A gate is declared, not derived: nothing here is computed from the rule's `query`, and
 * nothing checks the two against each other. So a gate can change what the rule reports — a
 * file it rejects is a file the rule never runs on, and a violation there is never found.
 *
 * A gate is neutral when it admits every file the rule would have reported on — yours to
 * keep, and not something the engine can check. The safe way to keep it is to gate wider
 * than the query, which is sufficient rather than necessary: a rule whose handler filters
 * may gate far narrower and still be neutral. `--profile` prints, per rule, how many files
 * each gate rejected and how many the rule actually parsed, which is where a suspected gate
 * is settled. A nonzero `cached` means the columns to its right are
 * incomplete for that run, since a cache hit returns before the content gates are consulted;
 * pair `--profile` with `--no-cache` to read them. `path-gated` is unaffected, because a path
 * gate runs before the cache is consulted at all.
 */
export interface Gates {
  /**
   * Glob patterns a file's path must match for the rule to consider it.
   *
   * The path is relative to the project root, and the pattern must match the whole path —
   * anchored, not a substring search. Patterns use the `globset` dialect, matched
   * case-sensitively: `*` matches any run of characters (including `/`), `?` any single
   * character, `[ab]`/`[!ab]` character classes and `{a,b}` alternates work, and `**`
   * recurses directories — `src/**` admits everything under `src`, and `**` in front
   * of `*.test.ts` admits a test file at any depth.
   */
  pathMatches?: string[]
  /**
   * Glob patterns that skip a file — a path matching any of these is never parsed. Checked
   * before `pathMatches` and winning over it: a path a `pathMatches` pattern would have
   * admitted is still skipped when a `pathNotMatches` pattern matches it.
   *
   * Same dialect and anchoring as `pathMatches`.
   */
  pathNotMatches?: string[]
  /**
   * Literal substrings a file's raw bytes must contain. A file missing any one of them is
   * never parsed.
   *
   * **This is an *and*, not an *or*.** A rule matching either of two tokens cannot express
   * its gate as `['a', 'b']` — that rejects any file containing only one, which is usually
   * most of them, and the rule then reports nothing while looking healthy. There is no `or`
   * form; omit the gate when no single substring covers every case.
   */
  fileContains?: string[]
  /**
   * Literal substrings that skip a file — a file whose raw bytes contain **any** of them is
   * never parsed. The mirror image of `fileContains`'s *and*: where that gate requires every
   * listed substring, this one rejects on the first that is present.
   */
  fileNotContains?: string[]
}

/** A replacement a rule offers for a violation. */
export interface Fix {
  /** The node whose text is replaced. */
  node: Node
  /** What to replace it with. */
  text: string
  /**
   * Whether the fix preserves behavior.
   *
   * Only a fix marked `true` is applied by `--fix`. Anything else is a suggestion — shown,
   * never written — because the cautious mistake costs a manual edit and the other one
   * rewrites someone's code silently.
   */
  safe?: boolean
}

/** Options for a single report. */
export interface ReportOptions {
  /** Overrides the card's `message` for this one violation. */
  message?: string
  /** A replacement to offer. */
  fix?: Fix
}

/**
 * A fact a rule emits for the reduce phase.
 *
 * `kind` is required and must be non-empty, because it is what `ctx.facts('...')` filters on.
 * A fact without one could never be retrieved, so emitting it is always a mistake — and a
 * silent one, since the rule would look like it was working right up until `reduce` found
 * nothing. lanekeep throws rather than accept it.
 */
export interface Fact {
  kind: string
  [key: string]: unknown
}

/** A fact as `reduce` receives it, with the file that emitted it. */
export interface EmittedFact extends Fact {
  /** Path of the file this came from, relative to the project root. */
  file: string
}

/**
 * A node's location: the file, line and column `ctx.loc` returns.
 *
 * `line` and `column` are required here, unlike on `ReduceLocation`: `ctx.loc` either
 * resolves the node and returns all three together, or the node does not resolve and the
 * call returns `undefined` entirely — there is no partial state to leave room for.
 */
export interface NodeLocation {
  /** Path relative to the project root. */
  file: string
  /** One-based. */
  line: number
  /** One-based. */
  column: number
}

/**
 * A subtree's structural fingerprint: identifiers and literal values erased.
 *
 * Computed host-side in one walk, so a rule does not pay a per-node boundary crossing to
 * inspect a tree's shape. Two functions differing only in identifier names, literal values
 * or comments hash identically; differing in an operator or a statement, differently. A
 * dead handle yields `undefined`, like `kind` and `loc`.
 */
export interface StructureFingerprint {
  /** blake3 of the normalized fold, lowercase hex. */
  hash: string
  /** How many nodes the fold covered — the thresholding input. */
  nodes: number
}

/**
 * Where a name came from. Returned by {@link TypeApi.symbolOf} directly, and nested under a
 * {@link TypeInfo} whose `symbol` field is set.
 */
export interface SymbolInfo {
  /**
   * The name as it appears at the use site, not at the declaration. For a renamed import —
   * `import { Decimal as Money }` — this is the local alias `Money`, never the exported
   * name `Decimal`. Comparing this field against an expected export name therefore rejects
   * a renamed import of the right type; `module` is the reliable field for "did this come
   * from there".
   */
  name: string
  /**
   * The module it was imported from. Absent for a local declaration — that absence is what
   * distinguishes an imported `Decimal` from a local class that happens to share the name.
   */
  module?: string
}

/**
 * What the oracle established about an expression, from {@link TypeApi.typeOf}.
 *
 * At most one of `primitive`, `symbol` or `union` is set, matching which kind of type this
 * is — a `union`'s members are already flattened one level and in canonical order. `text` is
 * set alongside whichever it is, but it is **display-only**: what TypeScript itself would
 * call the type, for a message a rule builds. Branch on `primitive` and `symbol`, never on
 * `text`'s wording.
 *
 * All three can be unset at once: a *nominal* type whose name the resolver could not
 * attribute — an unresolvable, global or ambient type such as `Date` used with no local
 * declaration or import — carries only `text`. That is not a gap to code around; it is
 * another shape of the same "I could not be sure" answer this whole surface is built on,
 * the same posture `typeOf` itself takes by returning `undefined` rather than guessing. Do
 * not assume the final branch of `if (primitive) … else if (symbol) … else` is unreachable
 * — for this shape, it is not.
 *
 * There is deliberately no `complete` field. Nothing in this milestone can make the oracle's
 * answer partial, and a field that never varies would only teach a rule to stop checking it.
 */
export interface TypeInfo {
  /** What TypeScript would call this type. Display-only — branch on the fields below instead. */
  text: string
  /** Set when this is a primitive — exactly the set TypeScript itself recognizes as one. */
  primitive?: 'number' | 'string' | 'boolean' | 'bigint' | 'symbol' | 'null' | 'undefined'
  /**
   * Set when this is a named type and the oracle could resolve where the name came from.
   * Absent on an unresolvable, global or ambient nominal type — see the interface doc above.
   */
  symbol?: SymbolInfo
  /** Set when this is a union. */
  union?: TypeInfo[]
}

/**
 * The bounded within-file type oracle, reached through `ctx.types`.
 *
 * Every question can come back with no answer, and no answer is a first-class result rather
 * than a failure to work around: the oracle is conservative on purpose, and it would rather
 * say nothing than say something wrong, because a rule reporting on a wrong type accuses
 * correct code. A rule is expected to check for `undefined` and quietly stay silent, the same
 * posture the rest of the navigation surface already takes on a dead handle.
 */
export interface TypeApi {
  /**
   * The type of the expression at `n`. `undefined` is that first-class no-answer, not a
   * failure.
   */
  typeOf(n: Node): TypeInfo | undefined
  /** Where the identifier at `n` was declared. `undefined` on the same terms as `typeOf`. */
  symbolOf(n: Node): SymbolInfo | undefined
}

/** A rule's RuleContext surface. */
export interface RuleContext {
  readonly filePath: string
  readonly fileText: string
  readonly root: Node
  kind(n: Node): string | undefined
  text(n: Node): string | undefined
  isNamed(n: Node): boolean
  line(n: Node): number | undefined
  column(n: Node): number | undefined
  parent(n: Node): Node | undefined
  children(n: Node): Node[]
  namedChildren(n: Node): Node[]
  ancestors(n: Node): Node[]
  structureFingerprint(n: Node): StructureFingerprint | undefined
  resolvesToImport(n: Node, module: string, name?: string): boolean
  isImportedFrom(n: Node, pattern: string): boolean
  bindingKind(n: Node): BindingKind | undefined
  isShadowed(n: Node): boolean
  querySubtree(n: Node, query: string): Match[]
  closestAncestor(n: Node, query: string): Match | undefined
  readFile(path: string): string | undefined
  fileExists(path: string): boolean
  emitFact(fact: Fact): void
  loc(n: Node): NodeLocation | undefined
  report(at: Node, message?: string | ReportOptions): void
  /** Facts emitted so far, optionally filtered by `kind`. */
  facts(kind?: string): EmittedFact[]
  /**
   * The bounded within-file type oracle, present only for a rule that declared
   * `requires: ['types']`.
   *
   * Typed as always present because there is no way to spell "present when
   * this rule's own `requires` says so" as a type — so a rule that forgets the
   * declaration still compiles. It finds out at the first call instead:
   * `ctx.types` is `undefined` at run time, and `ctx.types.typeOf(...)` throws a
   * `TypeError` rather than returning a quietly wrong answer. That loudness is
   * deliberate.
   */
  types: TypeApi
}

/** A violation the reduce phase reports, which has no node to point at. */
export interface ReduceLocation {
  /** Path relative to the project root. */
  file: string
  /** One-based. */
  line?: number
  /** One-based. */
  column?: number
}

/** A rule's ReduceContext surface. */
export interface ReduceContext {
  readonly files: string[]
  facts(kind?: string): EmittedFact[]
  report(at: ReduceLocation, message?: string | ReportOptions): void
}

/** A rule, as `defineRule` takes it. */
export interface Rule {
  /**
   * Namespaced identifier, as `namespace/name`.
   *
   * `local/` needs no declaration and `lanekeep/` is reserved for built-ins; any other
   * namespace must be listed in the config's `namespaces`.
   */
  id: string
  /**
   * Which languages this rule applies to.
   *
   * **Defaults to `['typescript', 'tsx']`**, and this is the field most worth getting right
   * on a rule for anything else. The grammar is chosen by the file, not by the rule, and a
   * rule does not run on a file whose language it does not name — so omitting this on a Go
   * or Rust rule means it silently never fires.
   */
  language?: LanguageId | LanguageId[]
  /**
   * Host analyses this rule needs before it can run.
   *
   * Absent means none, which is every rule today. A rule declaring one the engine cannot
   * provide is refused at load rather than run without it: an analysis that silently goes
   * missing makes the rule report nothing, and a rule reporting nothing is indistinguishable
   * from a codebase with nothing to report.
   */
  requires?: Array<'types' | 'dataflow'>
  /** How serious a violation is, before any config override. */
  severity: Severity
  /** What the rule tells whoever has to act on it. */
  card: RuleCard
  /** Cheap rejections before parsing. */
  gates?: Gates
  /**
   * The tree-sitter query gating the handler.
   *
   * Rust matches it across a single shared parse and only matches reach `check`, which is
   * what keeps a JavaScript rule affordable. Write the narrowest query that captures what
   * you need; `check` then only refines.
   *
   * A single string applies to every declared language. An object maps each declared
   * language to its own query — required when the grammars do not share node vocabulary
   * (Python spells a call `call`, the other supported grammars say `call_expression`).
   * Every declared language must have an entry and every entry must name a declared
   * language; a mismatch is a config-load error naming the language.
   *
   * Text predicates filter matches in Rust before the handler, so a predicate can only
   * narrow, never widen, what `check` sees: `#eq?`, `#not-eq?`, `#match?`, `#not-match?`,
   * `#any-of?` and `#not-any-of?` are supported (plus the `any-` forms of `eq?`/`match?`).
   * `#match?`/`#not-match?` run on the `regex` crate, which is deterministic and supports
   * no backreferences or lookaround. `#is?`, `#is-not?`, `#set!`, or an operator the
   * binding does not know is refused at compile time.
   */
  query: string | Partial<Record<LanguageId, string>>
  /** A per-invocation budget overriding the default, in milliseconds. */
  timeout?: number
  /** Called once per query match. */
  check?(ctx: RuleContext, match: Match): void
  /** Called once per run, after every file, with facts only. */
  reduce?(ctx: ReduceContext): void
}

/** A lanekeep configuration, as `defineConfig` takes it. */
export interface Config {
  /** Globs selecting files to check, relative to the project root. */
  include?: string[]
  /** Globs removing files from that selection. */
  exclude?: string[]
  /** Rule-id namespaces this project uses beyond `local`. */
  namespaces?: string[]
  /** Override a rule's own severity, by id. */
  severity?: Record<string, Severity>
  /** Execution budgets, in milliseconds. */
  timeouts?: {
    /** Per rule invocation. */
    rule?: number
    /** Wall-clock, for the whole run. */
    global?: number
  }
  /** Policy for suppression directives. All off by default. */
  suppressions?: {
    /** A valid directive with no `expires:` is reported. */
    requireExpiry?: boolean
    /** An expiry more than this many days after today is reported. */
    maxExpiryDays?: number
    /** Any whole-file directive is reported. */
    forbidFileScope?: boolean
  }
  /** The rules to run, in order. */
  rules: Rule[]
}

/**
 * Define a rule.
 *
 * An identity function. It exists so the compiler checks the object against {@link Rule}
 * where it is written, rather than reporting a mismatch from wherever it is imported.
 */
export declare function defineRule(rule: Rule): Rule

/**
 * Define a configuration.
 *
 * An identity function, for the same reason as {@link defineRule}. Most projects will write
 * `lanekeep.json` instead — configuration is data, and only rules need to be programs.
 */
export declare function defineConfig(config: Config): Config
