import { defineRule } from 'lanekeep'
import { appliesTo, matches } from 'lanekeep/patterns'

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
