import { defineRule } from 'lanekeep'

/**
 * One implementation written twice.
 *
 * An agent that cannot see the whole corpus reimplements a helper that already exists, in
 * every individual file the result looks fine, and no per-file rule can notice. This rule
 * fingerprints every function body — identifiers and literal values erased, computed
 * host-side by `ctx.structureFingerprint` — and reports any two bodies with the same shape.
 *
 * Matching is by structure, not by text: renaming an identifier or changing a literal value
 * still matches (that is the point), while changing an operator or adding a statement does
 * not. Comments never matter — the fingerprint is computed over the body with comments
 * erased, so in TypeScript a with-docstring/without-docstring pair has the same shape and is
 * flagged like any other pair. A *python* docstring is a statement rather than a comment, so
 * there the two directions pull apart: differing docstrings still group (the string's text is
 * erased), while with-docstring against without differs by a statement and does not.
 *
 * One rule, five grammars: `language` names them and `query` carries one entry per grammar.
 * Grouping never crosses a language — parallel implementations in two languages have
 * different interior node kinds, so their bodies cannot share a fingerprint.
 *
 * The `minNodes` threshold keeps tiny bodies (getters, one-line callbacks) out: a default of
 * 40 fires on real helpers, not on every two-line pair.
 *
 * Like every cross-file rule, it is skipped under `--since`/`--staged` with a notice on
 * stderr (docs/architecture.md §8.4) — it reports on full runs only.
 *
 * @example
 * ```ts
 * import duplicateImplementation from 'lanekeep/duplicate-implementation'
 *
 * export default defineConfig({
 *   rules: [duplicateImplementation({ minNodes: 60 })],
 * })
 * ```
 */
export default function duplicateImplementation(options) {
  // The ignored-options trap: options reach a rule only by being closed over. A handler is
  // invoked with two arguments; a third parameter would be `undefined` on every call.
  const minNodes = options?.minNodes ?? 40

  return defineRule({
    id: 'lanekeep/duplicate-implementation',
    language: ['typescript', 'tsx', 'python', 'go', 'rust'],
    severity: 'error',

    card: {
      message: 'duplicated implementation',
      remediation:
        'delete one copy and import the other — two bodies with one shape are one change that has to happen twice',
      examples: {
        bad: 'function normalize(raw) { return raw.trim().toLowerCase() }\nfunction clean(input) { return input.trim().toLowerCase() }',
        good: 'import { normalize } from "./normalize"',
      },
    },

    // The whole corpus feeds one pass, so the default per-invocation budget is the wrong
    // shape for it. See docs/cross-file-rules.md.
    timeout: 5_000,

    // One query per grammar it names. tsx shares the typescript node vocabulary, so those
    // two entries are the same string; the other three grammars spell their function forms
    // differently — python's `call` against everyone else's `call_expression` is the usual
    // example — which is what the per-language map exists for. The fingerprint is rooted at
    // the *body*, so within a language a method and a function of one shape still group.
    //
    // The `body:` field of `arrow_function` is `statement_block | expression`; matching the
    // block form alone is what keeps expression-bodied one-liners out. Python lambdas and
    // rust closures are excluded on the same reasoning: expression bodies are noise at any
    // threshold.
    query: {
      typescript: TS_QUERY,
      tsx: TS_QUERY,
      python: `
        (function_definition
          name: (identifier) @name
          body: (block) @body) @def
      `,
      go: `
        (function_declaration
          name: (identifier) @name
          body: (block) @body) @def

        (method_declaration
          name: (field_identifier) @name
          body: (block) @body) @def
      `,
      rust: `
        (function_item
          name: (identifier) @name
          body: (block) @body) @def
      `,
    },

    check(ctx, m) {
      const fp = ctx.structureFingerprint(m.body)
      if (!fp) return
      ctx.emitFact({
        kind: 'impl',
        hash: fp.hash,
        nodes: fp.nodes,
        line: ctx.line(m.def),
        column: ctx.column(m.def),
        name: m.name === undefined ? undefined : ctx.text(m.name),
      })
    },

    reduce(ctx) {
      const groups = new Map()
      for (const fact of ctx.facts('impl')) {
        if (fact.nodes < minNodes) continue
        const group = groups.get(fact.hash)
        if (group) {
          group.push(fact)
        } else {
          groups.set(fact.hash, [fact])
        }
      }

      for (const group of groups.values()) {
        if (group.length < 2) continue
        group.sort(byPosition)
        for (const member of group) {
          // `others` is the group minus this member, already sorted.
          const others = group.filter((fact) => fact !== member)
          ctx.report(
            { file: member.file, line: member.line, column: member.column },
            describe(member, others),
          )
        }
      }
    },
  })
}

/** The typescript grammar's function forms; tsx shares the vocabulary, so both entries use it. */
const TS_QUERY = `
  (function_declaration
    name: (identifier) @name
    body: (statement_block) @body) @def

  (method_definition
    body: (statement_block) @body) @def

  (function_expression
    body: (statement_block) @body) @def

  (arrow_function
    body: (statement_block) @body) @def
`

/** Facts in a stable order, so a message names the same counterparts every run. */
function byPosition(a, b) {
  if (a.file < b.file) return -1
  if (a.file > b.file) return 1
  return a.line - b.line || a.column - b.column
}

/**
 * The message for one member of a duplicate group.
 *
 * `others` is sorted; the first three are named, and the remainder collapsed into
 * "and N more" so the message stays bounded however large the group grows.
 */
function describe(member, others) {
  const shown = others.slice(0, 3)
  const list = shown.map((o) => `${o.file}:${o.line}`).join(', ')
  const rest = others.length - shown.length
  const capped = rest > 0 ? `${list} and ${rest} more` : list
  const prefix =
    member.name === undefined
      ? 'duplicated implementation — also at'
      : `'${member.name}' duplicates the implementation at`
  return `${prefix} ${capped}`
}
