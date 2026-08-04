import { defineRule } from 'lanekeep'

/**
 * Named exports only.
 *
 * A default export has no canonical name: every importer picks its own, so the same symbol
 * appears as three different identifiers across a codebase and neither grep nor a rename
 * refactor finds all of them. That cost is invisible in any single file, which is exactly
 * why it needs a rule rather than a review comment.
 */
export default defineRule({
  id: 'lanekeep/no-default-export',
  language: ['typescript', 'tsx'],
  severity: 'error',

  card: {
    message: 'default export',
    remediation:
      'use a named export, so the symbol has one name every importer must use',
    examples: {
      bad: 'export default function parse() {}',
      good: 'export function parse() {}',
    },
  },

  // A file whose bytes never contain `default` cannot have a default export in any of its
  // forms. Gating on `export default` would be tighter and wrong — it misses
  // `export { x as default }`, which is a default export written without those two words
  // adjacent.
  gates: {
    fileContains: ['default'],
  },

  // Three forms, because `default` is a keyword in one and an ordinary name in the others:
  //
  //   export default parse            the keyword form
  //   export { parse as default }      aliased to default
  //   export { default } from './x'    a default re-exported under the same name
  //
  // What decides the last two is the *exported* name — the alias when there is one. That is
  // why `export { default as parse }` is fine: it takes a default in and publishes a name.
  query: `
    (export_statement "default") @stmt

    (export_specifier alias: (_) @exported)

    (export_specifier !alias name: (_) @exported)
  `,

  check(ctx, m) {
    if (m.stmt) {
      ctx.report(m.stmt)
      return
    }

    if (ctx.text(m.exported) === 'default') {
      ctx.report(m.exported)
    }
  },
})
