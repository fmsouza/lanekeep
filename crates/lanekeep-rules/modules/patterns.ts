/**
 * Path-pattern matching shared by the restricted-import and restricted-calls rules.
 *
 * A shared module rather than a copy in each rule that needs it: the two rules
 * interpret a restriction's `*` glob and its `from` carve-outs the same way because
 * they share the code, not because each got it right independently.
 */

/**
 * Whether a path list applies to this file.
 *
 * An entry beginning with `!` is a carve-out: the restriction applies everywhere *except*
 * there. That inversion is what makes "nothing may import Stripe outside packages/payments"
 * expressible as one restriction rather than an enumeration of every other directory.
 *
 * No list at all means the restriction applies everywhere.
 */
export function appliesTo(from, file) {
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
export function matches(pattern, text) {
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
