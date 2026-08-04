import { defineRule } from 'lanekeep'

/**
 * A node handle tested for truthiness.
 *
 * Nodes cross into the sandbox as integer handles rather than as objects — one of the one-way
 * doors in §14 — and the root's handle is `0`. So the ordinary JavaScript spelling,
 * `const parent = ctx.parent(n); if (!parent) return`, treats every top-level item as
 * parentless.
 *
 * `crates/lanekeep-rules/rules/no-unwrap.ts` lost its whole `#[test]` exemption to exactly
 * this, silently, because the check it skipped only ever *removed* violations — a rule that
 * reports too much is noticed immediately, and one that reports too little is not noticed at
 * all.
 *
 * `closestAncestor` is deliberately not in the list: §6.2 says it returns `undefined` rather
 * than an empty object precisely so that `!` works on it.
 */
const NODE_RETURNING = ['parent', 'root']

export default defineRule({
  id: 'local/node-handle-truthiness',
  language: ['typescript', 'tsx'],
  severity: 'error',

  card: {
    message: 'node handle tested for truthiness',
    remediation: 'compare against `undefined` — the root node is handle `0`, which is falsy and is not absent',
    examples: {
      bad: 'const p = ctx.parent(n); if (!p) return',
      good: 'const p = ctx.parent(n); if (p === undefined) return',
    },
  },

  gates: { fileContains: ['ctx.parent'] },

  query: `
    (variable_declarator
      name: (identifier) @name
      value: (call_expression
        function: (member_expression
          property: (property_identifier) @method))) @decl
  `,

  check(ctx, m) {
    if (!NODE_RETURNING.includes(ctx.text(m.method))) return
    const name = ctx.text(m.name)

    const scope = ctx.closestAncestor(m.decl, '(statement_block) @block')
    if (scope === undefined) return

    for (const hit of ctx.querySubtree(
      scope.block,
      '(unary_expression argument: (identifier) @id) @neg',
    )) {
      if (ctx.text(hit.id) !== name) continue
      if (!ctx.text(hit.neg).startsWith('!')) continue

      ctx.report(hit.neg, {
        message: `\`${name}\` holds a node handle and the root's is \`0\`, so \`!${name}\` discards it`,
      })
    }
  },
})
