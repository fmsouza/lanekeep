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
 * A bare `m.something` as a fact value is the shape of that mistake, and the check is keyed on
 * that literal prefix — the conventional name of `check`'s second parameter, not binding
 * analysis. `options.kind` and `config.name` are member expressions too, and serialize
 * perfectly; they are left alone because their receiver isn't `m`. Same trade
 * `reduce-touches-no-tree.ts` makes for `ctx.`: an author who renames or destructures the
 * second parameter — `check(ctx, match)`, `check(ctx, { decl })` — defeats this rule silently.
 *
 * A second, narrower gap: `ctx.emitFact({ kind: 'e', decl })` stores `decl` by object-literal
 * shorthand. At that call site `decl` is a bare identifier, not a member expression, so this
 * rule has nothing to report even if `decl` was assigned from `m.decl` a line earlier — seeing
 * that would mean resolving the identifier back to its initializer, a binding-resolution pass
 * this rule does not attempt anywhere else.
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

      // Rooted at the match, not at anything else. `options.kind` and `config.name` are
      // member expressions too and serialize perfectly; only the match's captures are handles.
      // Keyed on the conventional parameter name the way `reduce-touches-no-tree` keys on
      // `ctx.` — an author who renames or destructures the second parameter defeats it, which
      // is the same accepted limitation for the same reason.
      if (!ctx.text(value).startsWith('m.')) continue

      ctx.report(pair, {
        message: `\`${ctx.text(value)}\` is a node handle; a fact has to survive JSON.stringify and be meaningful after the tree is gone`,
      })
    }
  },
})
