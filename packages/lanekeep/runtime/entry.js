/**
 * The seven exports `world rule` declares, over a table of rules.
 *
 * A component built from TypeScript rules is this module plus a generated file that imports
 * the rules and hands them to {@link register}. The generated half is the only part that
 * differs between one component and another, and it is three lines:
 *
 * ```js
 * import { register } from 'lanekeep/runtime/entry'
 * import noDefaultExport from './rules/no-default-export.ts'
 * register([noDefaultExport])
 * export * from 'lanekeep/runtime/entry'
 * ```
 *
 * **The runtime import comes first, and that ordering is load-bearing.** Importing this module
 * runs `withhold()` at its top level, which is what takes `Date`, `Math.random`, `fetch` and
 * the rest away from rule code — see `host.js` for the measurement that makes it necessary. ES
 * modules evaluate depth-first in source order, so a generated entry that imported a rule
 * *before* this one would let that rule's module scope capture a reference to the frozen clock
 * before it was withheld. Nothing in a rule's body is affected either way, because `check` and
 * `reduce` run long after both; module scope is the whole of the window, and writing the
 * runtime import first closes it.
 *
 * # A rule is an object or a factory, and both have to work
 *
 * `no-default-export` exports a rule object. `no-restricted-imports` exports a function taking
 * options and returning one, because the restriction *is* the rule and configuring it is not a
 * severity tweak. A component cannot close over a host-supplied value the way a factory does
 * in JavaScript — the options arrive later, as JSON, through `configure` — so a factory is
 * applied twice here: once at registration with no options, to learn its id, and again when
 * `configure` arrives with the real ones.
 *
 * Reading the id from an unconfigured application is sound because **a rule's id cannot depend
 * on its options**: the id is how a config names the rule, so it has to be knowable before the
 * rule is configured. `world rule` splits `rules` from `metadata` for that same reason, and
 * {@link configure} rejects a factory that changes its id rather than letting the two drift.
 */

import { buildCheckContext, buildReduceContext, toMatch, withhold } from './host.js'

// Before anything else in the component, including any rule module a generated entry imports
// after this one. The return value is the names that could not be deleted; a non-empty one
// means the sandbox has a hole, so it fails the build rather than shipping.
const survivors = withhold()
if (survivors.length > 0) {
  throw new Error(
    `these globals could not be withheld from rule code: ${survivors.join(', ')} — a rule ` +
      'compiled against this runtime would reach them, so the component is not built',
  )
}

/**
 * The rules this component hosts, in the order `rules()` enumerates them.
 *
 * @type {{id: string, source: object | Function, rule: object}[]}
 */
const table = []

/**
 * The default a rule that names no language gets.
 *
 * The grammar is chosen by the file and not by the rule, and a rule does not run on a file
 * whose language it does not name — so this default is what makes an unannotated rule a
 * TypeScript rule rather than a rule that silently never fires.
 */
const DEFAULT_LANGUAGES = ['typescript', 'tsx']

/**
 * Register the rules a component hosts. Called once, at build time, by the generated entry.
 *
 * Throws rather than reporting, deliberately: this runs inside `wizer` while the component is
 * being built, so a malformed rule fails the *build*. There is no error channel on `rules()`
 * or `metadata()` to report it through later, and a component that traps at prepare time tells
 * a user nothing about which rule was wrong.
 *
 * # Everything {@link metadata} reads without checking is checked here
 *
 * `metadata` returns a record rather than a result, so a missing `card` is a `TypeError` on
 * `undefined.message` **inside a wasm call** — which reaches the host as `wasm trap:
 * unreachable` with the message, the type and the stack gone, naming neither the field nor the
 * rule. The build is the only place that failure can be made legible, and it is also the only
 * place it can be made *early*: a rule that traps at prepare time has already shipped.
 *
 * Shape only, and deliberately not validity. Whether `severity` is one of the three lanekeep
 * accepts, whether `query` parses, whether the id's namespace is declared — all of that is
 * `lanekeep-config`'s `build_rule`, which holds a component to exactly the same rules as a
 * TypeScript module and is where the diagnostics for it already live. Restating any of it here
 * would be a second opinion that can disagree.
 *
 * @param {(object | Function)[]} entries Rule objects, factories, or a mix.
 */
export function register(entries) {
  if (!Array.isArray(entries)) {
    throw new TypeError('register expects an array of rules')
  }

  for (const [index, source] of entries.entries()) {
    const rule = apply(source, undefined, index)
    const id = rule?.id

    if (typeof id !== 'string' || id.length === 0) {
      throw new TypeError(
        `the rule at index ${index} has no \`id\` — a rule's id is how a config names it, so ` +
          'it cannot be omitted or produced from its options',
      )
    }

    // From here on the rule can be named, so every message does.
    const missing = []
    if (typeof rule.severity !== 'string' || rule.severity.length === 0) {
      missing.push('severity')
    }
    if (typeof rule.query !== 'string' || rule.query.length === 0) missing.push('query')

    const card = rule.card
    if (card === null || typeof card !== 'object') {
      missing.push('card')
    } else {
      if (typeof card.message !== 'string') missing.push('card.message')
      if (typeof card.remediation !== 'string') missing.push('card.remediation')

      const examples = card.examples
      if (examples === null || typeof examples !== 'object') {
        missing.push('card.examples')
      } else {
        if (typeof examples.bad !== 'string') missing.push('card.examples.bad')
        if (typeof examples.good !== 'string') missing.push('card.examples.good')
      }
    }

    if (missing.length > 0) {
      throw new TypeError(
        `\`${id}\` (index ${index}) is missing ${missing.join(', ')} — a rule declares all of ` +
          'these, and a component whose `metadata` cannot read one traps with nothing to say ' +
          'which rule or which field it was',
      )
    }

    table.push({ id, source, rule })
  }
}

/** Every rule this component hosts, by id, in index order. */
export function rules() {
  return table.map((entry) => entry.id)
}

/**
 * What one of this component's rules is, after it has been configured.
 *
 * Returns a record rather than a result, so anything wrong here traps. {@link register} is
 * where a malformed rule is caught, at build time, which is why this can read the fields
 * directly.
 *
 * @param {number} rule Index into the table.
 */
export function metadata(rule) {
  const { rule: defined } = at(rule)
  const language = defined.language
  const languages =
    language === undefined ? DEFAULT_LANGUAGES : Array.isArray(language) ? language : [language]

  return {
    id: defined.id,
    languages,
    severity: defined.severity,
    card: {
      message: defined.card.message,
      remediation: defined.card.remediation,
      examples: { bad: defined.card.examples.bad, good: defined.card.examples.good },
    },
    query: defined.query,
    gates: {
      // The published `Gates` type declares `fileContains` and nothing else, while the world
      // carries `lanekeep_core::Gates` field for field. The three a TypeScript rule cannot
      // express are sent empty rather than omitted: a WIT record has no absent field.
      pathMatches: [],
      pathNotMatches: [],
      fileContains: defined.gates?.fileContains ?? [],
      fileNotContains: [],
    },
    timeout: defined.timeout,
  }
}

/**
 * Hand one rule its configured options, once, after instantiation and before any `check`.
 *
 * @param {number} rule Index into the table.
 * @param {string} optionsJson The rule's options as JSON, or `"null"` when it was named with
 *   none — so a guest has one code path rather than two.
 * @returns {void} Returning normally is the `ok` case; a thrown string is the `err` case, and
 *   fails the run rather than trapping. A rule that cannot use its configuration has not
 *   merely misbehaved, it has been misconfigured, and the user needs to be told which.
 */
export function configure(rule, optionsJson) {
  const entry = at(rule)

  let options
  try {
    options = JSON.parse(optionsJson)
  } catch (failure) {
    throw `\`${entry.id}\` was given options that are not JSON: ${message(failure)}`
  }
  if (options === null) options = undefined

  if (typeof entry.source !== 'function') {
    // A rule object has nowhere to put options: they reach a rule by being closed over, which
    // is what a factory is for. Silently ignoring them is the failure `AGENTS.md` records
    // against `no-unwrap` and `no-glob-import` — an ignored option only ever *adds*
    // violations, and the author assumes their pattern is wrong.
    if (options === undefined) return
    throw `\`${entry.id}\` takes no options — it exports a rule, not a factory over one`
  }

  let configured
  try {
    configured = entry.source(options)
  } catch (failure) {
    throw `\`${entry.id}\` rejected its options: ${message(failure)}`
  }

  if (configured?.id !== entry.id) {
    // The id is what `rules()` already announced and what a config names. A factory whose id
    // moved with its options would make the two disagree, silently, from here on.
    throw (
      `\`${entry.id}\` changed its id to \`${configured?.id}\` when configured — a rule's id ` +
      'is how a config names it and cannot depend on its options'
    )
  }

  entry.rule = configured
}

/**
 * Whether this rule has a per-file pass.
 *
 * Probed once per rule at config load, and the answer is what the engine dispatches on.
 *
 * @param {number} rule Index into the table.
 */
export function hasCheck(rule) {
  return typeof at(rule).rule.check === 'function'
}

/**
 * Whether this rule has a cross-file pass.
 *
 * @param {number} rule Index into the table.
 */
export function hasReduce(rule) {
  return typeof at(rule).rule.reduce === 'function'
}

/**
 * Run one rule's per-file pass for one query match.
 *
 * @param {number} rule Index into the table.
 * @param {object} ctx The borrowed `check-context`.
 * @param {{name: string, node: number}[]} m The match's captures.
 */
export function check(rule, ctx, m) {
  const { rule: defined } = at(rule)
  try {
    defined.check(buildCheckContext(ctx), toMatch(m))
  } catch (failure) {
    throw ruleError(failure)
  }
}

/**
 * Run one rule's cross-file pass, once per run.
 *
 * @param {number} rule Index into the table.
 * @param {object} ctx The borrowed `reduce-context`.
 */
export function reduce(rule, ctx) {
  const { rule: defined } = at(rule)
  try {
    defined.reduce(buildReduceContext(ctx))
  } catch (failure) {
    throw ruleError(failure)
  }
}

/**
 * The table entry for an index, or a thrown refusal naming what was asked for.
 *
 * @param {number} rule
 */
function at(rule) {
  const entry = table[rule]
  if (entry === undefined) {
    throw new RangeError(
      `this component hosts ${table.length} rule(s) and was asked for index ${rule}`,
    )
  }
  return entry
}

/**
 * A rule source applied to options, whether it is a factory or already a rule.
 *
 * @param {object | Function} source
 * @param {unknown} options
 * @param {number} index Only for the message, which is the whole reason this is not inline.
 */
function apply(source, options, index) {
  if (typeof source === 'function') return source(options)
  if (source !== null && typeof source === 'object') return source

  throw new TypeError(
    `the rule at index ${index} is a ${typeof source} — a rule module's default export is a ` +
      'rule object or a factory returning one',
  )
}

/**
 * What a thrown value said, for a message that has to be a string.
 *
 * @param {unknown} failure
 */
function message(failure) {
  if (failure instanceof Error) return failure.message
  return String(failure)
}

/**
 * A thrown value as the world's `rule-error`.
 *
 * The graceful-failure channel, and it is not a second way of spelling a trap: an uncaught
 * throw inside a component arrives at the host as `wasm trap: unreachable` with the message,
 * the type and the whole stack gone, because there is nowhere for any of them to go.
 *
 * @param {unknown} failure
 * @returns {{message: string, frames: {function: string, file: string, line: number,
 *   column: number}[]}}
 */
function ruleError(failure) {
  return { message: message(failure), frames: frames(failure) }
}

/**
 * The frames of a thrown error's stack, innermost first.
 *
 * SpiderMonkey writes `name@file:line:column`, one frame per line, with the name empty for an
 * anonymous function. Positions are in the space the guest was compiled to — for a bundled
 * rule that is the bundle and not the author's TypeScript — and the host remaps them through
 * the component's source map before a user sees one.
 *
 * A line that does not parse is dropped rather than guessed at. `frames` is allowed to be
 * empty; a wrong position is worse than none, because it sends a reader to a real line that
 * has nothing to do with the failure.
 *
 * @param {unknown} failure
 */
function frames(failure) {
  const stack = failure instanceof Error ? failure.stack : undefined
  if (typeof stack !== 'string') return []

  const parsed = []
  for (const line of stack.split('\n')) {
    const trimmed = line.trim()
    if (trimmed.length === 0) continue

    const at = trimmed.lastIndexOf('@')
    if (at < 0) continue

    const where = trimmed.slice(at + 1)
    // From the right: a file name may contain a colon, a line and column may not.
    const lastColon = where.lastIndexOf(':')
    if (lastColon < 0) continue
    const firstColon = where.lastIndexOf(':', lastColon - 1)
    if (firstColon < 0) continue

    const row = Number(where.slice(firstColon + 1, lastColon))
    const col = Number(where.slice(lastColon + 1))
    if (!Number.isInteger(row) || !Number.isInteger(col)) continue

    parsed.push({
      function: trimmed.slice(0, at),
      file: where.slice(0, firstColon),
      line: row,
      column: col,
    })
  }
  return parsed
}
