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
 * import noGlobImport from 'lanekeep/no-glob-import'
 *
 * export default defineConfig({
 *   rules: [noGlobImport({ allow: ['**\/prelude', 'std::prelude::*'] })],
 * })
 * ```
 */
function build(options) {
  // Preludes are the one shape a glob is the intended spelling of, and a project that uses
  // one should not have to suppress this on every file. An explicit list replaces this
  // rather than extending it: a project that names its own exceptions has said what they are.
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
      // Already ends in `::*`. tree-sitter-rust's `use_wildcard` is `(path '::')? '*'`, so
      // the captured text carries the whole thing — appending another `::*` to build the
      // message produced `use crate::models::*::*`, which nothing caught because no test
      // asserted a message and the default pattern matches either spelling.
      const path = ctx.text(m.wildcard)

      for (const pattern of allow) {
        if (matches(pattern, path)) return
      }

      ctx.report(m.use, {
        message: `\`use ${path}\` hides where every name in this file comes from`,
      })
    },
  })
}

/**
 * Callable for options, usable bare for none.
 *
 * See `no-unwrap.ts` for why both shapes have to work and why this is not `Object.assign`.
 */
function noGlobImport(options) {
  return build(options)
}

const unconfigured = build(undefined)
for (const key in unconfigured) noGlobImport[key] = unconfigured[key]

export default noGlobImport

/** A `*` wildcard match, anchored at both ends. */
function matches(pattern: string, value: string): boolean {
  const escaped = pattern.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')
  return new RegExp(`^${escaped}$`).test(value)
}
