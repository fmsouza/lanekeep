/**
 * Type definitions for authoring lanekeep rules.
 *
 * These describe the host API a rule reaches inside lanekeep's sandbox. Nothing here runs in
 * Node: `defineRule` and `defineConfig` are identity functions whose only job is to give the
 * compiler something to check against, and `RuleContext` is provided by lanekeep at run time.
 *
 * Every member below is asserted against the host's own registration by
 * `local/host-api-matches-types`, one of this repository's own self-check rules
 * (`lanekeep/rules/host-api-matches-types.ts`). A method that exists here and not there — or
 * the reverse — is reported, because a definition that drifts from the engine is worse than
 * none: it produces confident autocomplete for something that does not exist.
 *
 * `BindingKind` specifically is checked the same way, by `local/binding-kinds-are-typed`
 * (`lanekeep/rules/binding-kinds-are-typed.ts`).
 */

/**
 * A node in the parse tree.
 *
 * Deliberately opaque. Nodes cross into the sandbox as integer handles rather than objects,
 * which is one of the one-way doors in the architecture — and the reason this is a branded
 * type rather than `number` is that **the root node's handle is `0`**. Written as a plain
 * number, `if (!node)` looks like a null check and silently discards the root, which is how
 * a rule loses its whole top-level case without any error.
 *
 * Compare against `undefined` explicitly.
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

/** Cheap rejections applied before a file is parsed. */
export interface Gates {
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

/** What a rule's `check` handler reaches. */
export interface RuleContext {
  /** Path of the file being checked, relative to the project root. */
  readonly filePath: string
  /** The whole file, as text. */
  readonly fileText: string
  /** The tree's root node. Its handle is `0` — see {@link Node}. */
  readonly root: Node

  /** The node's kind, as tree-sitter names it. */
  kind(node: Node): string
  /** The source text the node spans. */
  text(node: Node): string
  /** Whether this is a named node rather than an anonymous token. */
  isNamed(node: Node): boolean
  /** One-based line of the node's start. */
  line(node: Node): number
  /** One-based column of the node's start. */
  column(node: Node): number

  /** The node's parent, or `undefined` at the root. */
  parent(node: Node): Node | undefined
  /** Every child, including anonymous tokens. */
  children(node: Node): Node[]
  /** Named children only. */
  namedChildren(node: Node): Node[]
  /** Every ancestor, innermost first. */
  ancestors(node: Node): Node[]

  /**
   * Whether an identifier resolves to a given import.
   *
   * Handles aliasing, so `import { makeStyles as ms }` resolves correctly. This is the call
   * that separates a rule from a grep: a text match both misses the alias and fires on a
   * local of the same name.
   *
   * @param name Which export. Omit to match the module regardless of which name was taken.
   */
  resolvesToImport(node: Node, module: string, name?: string): boolean
  /** Whether an identifier came from a module matching this glob. */
  isImportedFrom(node: Node, pattern: string): boolean
  /** How the name was introduced, or `undefined` when it does not resolve. */
  bindingKind(node: Node): BindingKind | undefined
  /** Whether an outer binding of the same name is hidden by this one. */
  isShadowed(node: Node): boolean

  /** Run a query inside a subtree. */
  querySubtree(node: Node, query: string): Match[]
  /** The nearest ancestor matching a query, with its captures. */
  closestAncestor(node: Node, query: string): Match | undefined

  /**
   * Read another file, relative to the project root.
   *
   * Tracked: the read becomes part of the cache key, so a change to that file invalidates
   * this one's result. Confined to the project root; `undefined` when absent or outside.
   */
  readFile(path: string): string | undefined
  /** Whether a file exists, tracked the same way. */
  fileExists(path: string): boolean

  /** Emit a fact for the reduce phase. */
  emitFact(fact: Fact): void
  /** Facts emitted so far, optionally filtered by `kind`. */
  facts(kind?: string): EmittedFact[]

  /** The node's location, in the shape a fact carries and a reduce phase reports at. */
  loc(node: Node): NodeLocation | undefined
  /** Report a violation at a node. */
  report(at: Node, message?: string | ReportOptions): void
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

/**
 * What a rule's `reduce` handler reaches.
 *
 * Deliberately smaller than {@link RuleContext}: **the reduce phase never touches parse
 * trees.** Facts are small and serializable, which is what keeps cross-file rules parallel
 * and cacheable — handing a tree to `reduce` would make the whole corpus resident.
 */
export interface ReduceContext {
  /** Every file the run checked, relative to the project root. */
  readonly files: string[]
  /** Facts from every file, optionally filtered by `kind`. */
  facts(kind?: string): EmittedFact[]
  /** Report a violation against a file. */
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
   */
  query: string
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
