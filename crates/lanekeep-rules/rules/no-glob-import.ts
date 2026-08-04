import { defineRule } from 'lanekeep'

/**
 * `use foo::*;`
 *
 * A glob import makes it impossible to answer, by reading the file, where a name came from.
 * Every unqualified identifier becomes a candidate for every glob in scope, and the answer
 * moves when an upstream crate adds a public item — a name that resolved to yours last week
 * resolves to theirs today, with no change on your side.
 *
 * That is an architectural cost rather than a stylistic one: it makes the module's dependency
 * surface unreadable, and it is exactly the case that defeats tooling too. lanekeep's own
 * resolver reports nothing for a glob import, because the names it brings in cannot be known
 * without reading the other crate — so a rule asking "is this the imported `Result`?" quietly
 * stops being answerable in any file that has one.
 *
 * The prelude conventions are the exception worth allowing, and `allow` exists for them.
 *
 * @example
 * ```ts
 * export default defineConfig({
 *   rules: [{ rule: 'lanekeep/no-glob-import', options: { allow: ['*prelude*'] } }],
 * })
 * ```
 */
export default function noGlobImport(options) {
  const allow = options?.allow ?? ['*prelude*']

  return defineRule({
    id: 'lanekeep/no-glob-import',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'glob import',
      remediation: 'name what you import, so a reader can tell where each name comes from',
      examples: {
        bad: 'use crate::models::*;',
        good: 'use crate::models::{User, Session};',
      },
    },

    gates: { fileContains: ['use'] },

    query: '(use_declaration argument: (use_wildcard) @wildcard) @use',

    check(ctx, m) {
      const path = ctx.text(m.wildcard)

      // Preludes are the one shape a glob is the intended spelling of, and a project that
      // uses one should not have to suppress this on every file.
      for (const pattern of allow) {
        if (matches(pattern, path)) return
      }

      ctx.report(m.use, {
        message: `\`use ${path}::*\` hides where every name in this file comes from`,
      })
    },
  })
}

/** A `*` wildcard match, anchored at both ends. */
function matches(pattern: string, value: string): boolean {
  const escaped = pattern.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')
  return new RegExp(`^${escaped}$`).test(value)
}
