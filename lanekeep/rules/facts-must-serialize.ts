import { defineRule } from 'lanekeep'

/**
 * A node handle stored in a fact.
 *
 * §6.5: a fact "must survive `JSON.stringify`, because facts are cached". A node handle is an
 * integer index into a tree that exists only while its file is being checked — it serializes
 * perfectly and means nothing when read back, either in a later reduce phase or on a warm run
 * from the cache.
 *
 * §4 makes the same point about positions: they travel as plain numbers, and `reduce` reports
 * at `{ file, line, column }`, because the position has to be captured during the per-file
 * pass while the tree is still there.
 *
 * A bare `m.something` as a fact value is the shape of that mistake.
 */
export default defineRule({
  id: 'local/facts-must-serialize',
  language: ['typescript', 'tsx'],
  severity: 'error',

  card: {
    message: 'node handle stored in a fact',
    remediation: 'store `ctx.text`, `ctx.line` or `ctx.column` of the node — the handle is an index into a tree that will be gone',
    examples: {
      bad: "ctx.emitFact({ kind: 'export', node: m.decl })",
      good: "ctx.emitFact({ kind: 'export', name: ctx.text(m.decl), line: ctx.line(m.decl) })",
    },
  },

  gates: { fileContains: ['emitFact'] },

  query: `
    (call_expression
      function: (member_expression property: (property_identifier) @method)
      arguments: (arguments (object) @fact))
  `,

  check(ctx, m) {
    if (ctx.text(m.method) !== 'emitFact') return

    for (const pair of ctx.namedChildren(m.fact)) {
      if (ctx.kind(pair) !== 'pair') continue

      const parts = ctx.namedChildren(pair)
      const value = parts[parts.length - 1]
      if (ctx.kind(value) !== 'member_expression') continue

      ctx.report(pair, {
        message: `\`${ctx.text(value)}\` is a node handle; a fact has to survive JSON.stringify and be meaningful after the tree is gone`,
      })
    }
  },
})
