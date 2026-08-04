import { defineRule } from 'lanekeep'

/**
 * Forbid importing given modules, optionally only from given paths.
 *
 * The workhorse architectural rule: "the UI layer must not reach into the database
 * client", "nothing outside `payments/` may import the Stripe SDK". Those are exactly the
 * conventions a language model cannot infer from the code it is shown.
 *
 * A factory rather than a rule object, because the restriction is the whole content of the
 * rule. Configuring it is not a severity tweak — a caller supplies what is forbidden and
 * why, and gets a rule back.
 *
 * @example
 * ```ts
 * import noRestrictedImports from 'lanekeep/no-restricted-imports'
 *
 * export default defineConfig({
 *   rules: [
 *     noRestrictedImports({
 *       restrictions: [
 *         { module: 'stripe', from: ['!packages/payments/**'], reason: 'route it through the payments package' },
 *         { module: 'lodash/*', reason: 'use the standard library' },
 *       ],
 *     }),
 *   ],
 * })
 * ```
 */
export default function noRestrictedImports(options) {
  const restrictions = options?.restrictions ?? []

  return defineRule({
    id: 'lanekeep/no-restricted-imports',
    language: ['typescript', 'tsx'],
    severity: 'error',

    card: {
      message: 'restricted import',
      remediation: 'import something permitted here, or move this code where it is allowed',
      examples: {
        bad: "import Stripe from 'stripe'",
        good: "import { charge } from '@app/payments'",
      },
    },

    query: '(import_statement source: (string) @source) @stmt',

    check(ctx, m) {
      // The specifier as written, minus its quotes. Matching the raw text rather than a
      // resolved path is deliberate: a restriction is written against what an author
      // types, and resolving first would make `lodash/*` fail to match `lodash/merge`.
      const raw = ctx.text(m.source)
      const specifier = raw.slice(1, -1)
      const file = ctx.filePath

      for (const restriction of restrictions) {
        if (!matches(restriction.module, specifier)) continue
        if (!appliesTo(restriction.from, file)) continue

        const reason = restriction.reason
          ? `${restriction.reason}`
          : 'this import is not allowed here'
        ctx.report(m.stmt, `importing '${specifier}' is restricted — ${reason}`)
        return
      }
    },
  })
}

/**
 * Whether a path list applies to this file.
 *
 * An entry beginning with `!` is a carve-out: the restriction applies everywhere *except*
 * there. That inversion is what makes "nothing may import Stripe outside packages/payments"
 * expressible as one restriction rather than an enumeration of every other directory.
 *
 * No list at all means the restriction applies everywhere.
 */
function appliesTo(from, file) {
  if (!from || from.length === 0) return true

  const exemptions = from.filter((p) => p.startsWith('!'))
  const inclusions = from.filter((p) => !p.startsWith('!'))

  for (const exemption of exemptions) {
    if (matches(exemption.slice(1), file)) return false
  }
  if (inclusions.length === 0) return true
  return inclusions.some((pattern) => matches(pattern, file))
}

/**
 * Glob matching where `*` spans anything, including separators.
 *
 * Written out rather than imported, because rules run in a sandbox with no package
 * resolution — and because the need here is `lodash/*` and `packages/payments/**`, not a
 * full glob dialect.
 */
function matches(pattern, text) {
  if (pattern === text) return true

  const parts = pattern.split('*')
  if (parts.length === 1) return false

  if (!text.startsWith(parts[0])) return false
  let rest = text.slice(parts[0].length)

  for (let i = 1; i < parts.length; i += 1) {
    const segment = parts[i]
    if (segment === '') continue

    if (i === parts.length - 1) return rest.endsWith(segment)

    const at = rest.indexOf(segment)
    if (at === -1) return false
    rest = rest.slice(at + segment.length)
  }

  return true
}
