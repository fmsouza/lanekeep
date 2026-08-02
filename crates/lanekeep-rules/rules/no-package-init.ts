import { defineRule } from 'lanekeep'

/**
 * `func init()` at package level.
 *
 * An init function runs when the package is imported, before `main`, in an order the
 * language decides. Nothing calls it, so nothing in the code says when it happens; a reader
 * tracing startup finds no edge leading to it. Two packages that both register into a shared
 * map depend on an order neither one states, and the failure — a missing registration, a nil
 * global — surfaces far from the cause and moves when an unrelated import is added.
 *
 * It is also the usual way a package acquires hidden startup cost: an import that looks free
 * opens a connection or reads a file. That is what makes this architectural rather than
 * stylistic, and worth stating as a project convention rather than arguing case by case.
 *
 * Wiring done explicitly from `main` — or a `New...` returning an error — is traceable,
 * testable, and ordered by the code rather than by the linker.
 *
 * @example
 * ```ts
 * import noPackageInit from 'lanekeep/no-package-init'
 *
 * export default defineConfig({ rules: [noPackageInit] })
 * ```
 */
export default defineRule({
  id: 'lanekeep/no-package-init',
  language: 'go',
  severity: 'error',

  card: {
    message: 'package-level func init()',
    remediation:
      'move the work into an explicit constructor or a call from main, so the order it happens in is written down',
    examples: {
      bad: 'func init() {\n\tregistry["pg"] = newPostgres()\n}',
      good: 'func Register(r map[string]Driver) {\n\tr["pg"] = newPostgres()\n}',
    },
  },

  gates: { fileContains: ['init'] },

  // `function_declaration` is only ever package level in Go — a function inside a function is
  // a `func_literal`, which this cannot match. So matching the name is enough, and there is
  // no nesting check to get wrong.
  query: '(function_declaration name: (identifier) @name) @func',

  check(ctx, m) {
    if (ctx.text(m.name) !== 'init') return

    ctx.report(m.func, {
      message:
        '`init` runs at import time in an order nothing states, so what it sets up is untraceable from the code that depends on it',
    })
  },
})
