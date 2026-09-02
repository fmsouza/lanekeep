import { defineRule } from 'lanekeep'
import { matches } from 'lanekeep/patterns'

/**
 * Forbid a primitive type on values a convention says carry a domain type.
 *
 * The convention this exists for: "every monetary value is a `Decimal` from `decimal.js`,
 * and never a `number`, because a number loses precision past 2^53". That is not something
 * a language model can infer from the code it is shown — the syntax is clean either way —
 * and it is exactly what this tool is for.
 *
 * A factory rather than a rule object, for the reason `no-restricted-imports` is one: the
 * convention is the whole content of the rule. Hardcoding `decimal.js` would put one
 * project's policy in the tool.
 *
 * **Values are selected by name, and that is as good as the project's naming.** A monetary
 * value called `total` slips past `names: ['*amount*']`, and a `maxRetryAmount` is caught
 * wrongly. There is no inference here and none is claimed: the project is telling the tool
 * something it cannot work out.
 *
 * **The match is also case-sensitive**, the same as every other glob `lanekeep/patterns`
 * matches: `'*amount*'` alone misses `totalAmount`, because the pattern's lowercase `a`
 * never matches the capital one a camelCase name puts there. A convention meaning to catch
 * every casing lists both, as the example below does.
 *
 * A union type reports iff any of its members is a forbidden primitive, and stays silent
 * otherwise — never by falling through to the nominal check below. `Decimal | undefined`
 * is optional money and none of its members is a forbidden primitive, so it stays silent;
 * `number | Decimal` can still be a bare `number` at run time, so it reports.
 *
 * @example
 * ```ts
 * import noRestrictedTypes from 'lanekeep/no-restricted-types'
 *
 * export default defineConfig({
 *   rules: [
 *     noRestrictedTypes({
 *       conventions: [
 *         {
 *           names: ['*amount*', '*Amount*', '*balance*', '*price*'],
 *           forbid: ['number', 'string'],
 *           require: { module: 'decimal.js', name: 'Decimal' },
 *           reason: 'number loses precision past 2^53',
 *         },
 *       ],
 *     }),
 *   ],
 * })
 * ```
 */
export default function noRestrictedTypes(options) {
  const conventions = options?.conventions ?? []

  return defineRule({
    id: 'lanekeep/no-restricted-types',
    language: ['typescript', 'tsx'],
    severity: 'error',

    // Declared, so the engine hands this rule `ctx.types`. Without it the namespace is
    // absent and the first call throws — deliberately loud, rather than a silent
    // `undefined` that would make the rule report nothing and read as a clean file.
    requires: ['types'],

    card: {
      message: 'restricted type on a value the convention governs',
      remediation: 'give it the type the convention requires, or rename it if it is not what the name says',
      examples: {
        bad: 'function credit(amount: number)',
        good: 'function credit(amount: Decimal)',
      },
    },

    query: `[
      (required_parameter pattern: (identifier) @name)
      (optional_parameter pattern: (identifier) @name)
      (variable_declarator name: (identifier) @name)
    ]`,

    check(ctx, m) {
      const name = ctx.text(m.name)

      for (const convention of conventions) {
        const names = convention.names ?? []
        if (!names.some((pattern) => matches(pattern, name))) continue

        const type = ctx.types.typeOf(m.name)

        // The contract this rule exists to demonstrate. `undefined` means the oracle could
        // not be sure, and reporting on it would accuse code it could not read. Silence
        // here produces false negatives and never false positives.
        if (type === undefined) continue

        if (type.primitive !== undefined) {
          if (!(convention.forbid ?? []).includes(type.primitive)) continue
          ctx.report(m.name, { message: reasonFor(convention) })
          return
        }

        if (type.union !== undefined) {
          // A member-wise question, answered independently of the nominal branch below:
          // "does any member of this union type-check as a forbidden primitive?" A `continue`
          // here — silently accepting every union — would let a bare `number` hide behind
          // `number | Decimal`. Falling through to the nominal branch instead would report
          // on every union with no `symbol` of its own, which is every union: `Decimal |
          // undefined` is optional money and must stay silent, not a violation.
          const forbid = convention.forbid ?? []
          const hasForbiddenMember = type.union.some(
            (member) => member.primitive !== undefined && forbid.includes(member.primitive),
          )
          if (!hasForbiddenMember) continue
          ctx.report(m.name, { message: reasonFor(convention) })
          return
        }

        // A named type. It satisfies the convention only when it is the *required* one,
        // matched on the module its symbol came from rather than on its name — a local
        // `class Decimal {}` is not `decimal.js`'s, and matching by name would accept it.
        //
        // A nominal type the oracle could not attribute carries no symbol at all, so it
        // cannot match and is reported: a governed value whose type cannot be established
        // is not evidence the convention is met.
        if (convention.require === undefined) continue
        const symbol = type.symbol
        const satisfied =
          symbol !== undefined &&
          symbol.module === convention.require.module &&
          symbol.name === convention.require.name
        if (!satisfied) {
          ctx.report(m.name, { message: reasonFor(convention) })
          return
        }
      }
    },
  })
}

/**
 * What to tell the reader, preferring the convention's own words.
 *
 * `require` is optional, so the fallback cannot name a replacement type — which is why a
 * convention's `reason` earns its place rather than being decoration.
 */
function reasonFor(convention) {
  if (convention.reason !== undefined) return convention.reason
  if (convention.require !== undefined) {
    return `use ${convention.require.name} from ${convention.require.module}`
  }
  return 'this type is restricted on a value the convention governs'
}
