import { defineRule } from 'lanekeep'

/**
 * Forbid a primitive type in an argument position a convention governs.
 *
 * `no-restricted-types` selects values by name, and that is a real limitation: a project
 * convention that money is a `Decimal` cannot say anything about `new Decimal(parseFloat(x))`,
 * because `parseFloat(x)` is not a name at all — it is an argument position at a call site. This
 * rule selects the other way, by the callee a call resolves to and the position an argument
 * sits in, and reaches the case its sibling cannot.
 *
 * A factory rather than a rule object, for the same reason as `no-restricted-imports` and
 * `no-restricted-types`: the restriction is the whole content of the rule, and hardcoding
 * `decimal.js` would put one project's policy in the tool.
 *
 * **Declares `requires: ['types']`.** Without it the engine never hands this rule `ctx.types`,
 * the namespace is `undefined`, and the first call throws — deliberately loud, the same
 * contract `no-restricted-types` documents, rather than a silent `undefined` that would read as
 * a clean file.
 *
 * **The comment filter, with the measured evidence.** `ctx.namedChildren` of an `arguments` node
 * includes comments as ordinary named children: `new Decimal(/* cents *\/ parseFloat(x))` gives
 * `0:comment`, `1:call_expression`. Without filtering `kind === 'comment'` out first, a leading
 * comment shifts every later argument's index by one and a rule reading position 0 asks the
 * oracle about the comment, gets `undefined`, and goes silent on code it should have reported.
 *
 * **The default position is 0, not every argument.** `new Decimal(parseFloat(a), 10)` is the
 * case that rules this out: the `10` is a radix literal, it types as `number`, and a rule
 * checking every argument by default would accuse it alongside `parseFloat(a)`. A convention
 * that genuinely means every argument asks for that with `argument: 'all'`.
 *
 * **`undefined` from the oracle is a first-class answer and stays silent on it**, the same
 * contract as `no-restricted-types`: a member expression like `row.amount` or a spread element
 * types as `undefined`, and reporting on it would accuse code the oracle could not read. False
 * negatives are the price; false positives are the one failure this design forbids.
 *
 * **The callee is resolved through the import, not by name**, which is where this rule is more
 * precise than its sibling. `no-restricted-types`'s `require` had to give up name comparison for
 * exactly this reason — the oracle's `symbol.name` is the use-site name, so comparing it would
 * reject a conforming alias. `ctx.resolvesToImport` answers the question `no-restricted-types`
 * cannot ask at all: `import { Decimal as Money } from 'decimal.js'; new Money(...)` still
 * resolves to `decimal.js`'s `Decimal`, because the check follows the binding rather than the
 * text at the call site.
 *
 * **`call.name` is optional, and calling `ctx.resolvesToImport` with three arguments when it is
 * absent throws.** The host binding's third parameter is *arity*-optional, not
 * `undefined`-tolerant — passing an explicit `undefined` fails argument conversion at the
 * boundary, aborting the whole run rather than reporting or staying silent. `call.name` naming
 * nothing at all is the documented spelling for "govern every export of this module," so the
 * two-argument and three-argument calls are branched on explicitly here rather than always
 * passing three.
 *
 * **A union reports iff one of its members is a forbidden primitive**, decided the same way as
 * `no-restricted-types`'s union branch: `number | Decimal` can still be a bare `number` at run
 * time and reports, `Decimal | undefined` is optional money and stays silent. A nominal type
 * that is not a forbidden primitive is never a violation here — there is no `require` in this
 * rule's shape, only `forbid`.
 *
 * @example
 * ```ts
 * import noRestrictedArguments from 'lanekeep/no-restricted-arguments'
 *
 * export default defineConfig({
 *   rules: [
 *     noRestrictedArguments({
 *       restrictions: [
 *         {
 *           call: { module: 'decimal.js', name: 'Decimal' },
 *           forbid: ['number'],
 *           reason: 'construct a Decimal from a string, not a float',
 *         },
 *       ],
 *     }),
 *   ],
 * })
 * ```
 */
export default function noRestrictedArguments(options) {
  const restrictions = options?.restrictions ?? []

  return defineRule({
    id: 'lanekeep/no-restricted-arguments',
    language: ['typescript', 'tsx'],
    severity: 'error',
    requires: ['types'],

    card: {
      message: 'restricted type on an argument the convention governs',
      remediation: 'convert it before the call, or pass a value the callee is meant to take',
      examples: {
        bad: 'new Decimal(parseFloat(row.amount))',
        good: 'new Decimal(row.amount)',
      },
    },

    query: `[
      (new_expression  constructor: (identifier) @callee arguments: (arguments) @args)
      (call_expression function:    (identifier) @callee arguments: (arguments) @args)
    ]`,

    check(ctx, m) {
      for (const restriction of restrictions) {
        const call = restriction.call
        if (call === undefined) continue

        const resolved =
          call.name === undefined
            ? ctx.resolvesToImport(m.callee, call.module)
            : ctx.resolvesToImport(m.callee, call.module, call.name)
        if (!resolved) continue

        const args = ctx
          .namedChildren(m.args)
          .filter((node) => ctx.kind(node) !== 'comment')

        const forbid = restriction.forbid ?? []
        const positions =
          restriction.argument === 'all'
            ? args.map((_, index) => index)
            : [restriction.argument ?? 0]

        for (const position of positions) {
          const argument = args[position]
          if (argument === undefined) continue

          const type = ctx.types.typeOf(argument)
          if (type === undefined) continue
          if (!isForbidden(type, forbid)) continue

          ctx.report(argument, { message: reasonFor(restriction) })
          return
        }
      }
    },
  })
}

/**
 * A primitive is forbidden directly, or reachable through a union member — never through a
 * nominal type, which is a difference from `no-restricted-types`: this rule has no `require`,
 * so there is nothing for a named type to satisfy or fail.
 */
function isForbidden(type, forbid) {
  if (type.primitive !== undefined) return forbid.includes(type.primitive)
  if (type.union !== undefined) {
    return type.union.some(
      (member) => member.primitive !== undefined && forbid.includes(member.primitive),
    )
  }
  return false
}

/**
 * What to tell the reader, preferring the restriction's own words.
 *
 * There is no `require` here to fall back to naming, unlike `no-restricted-types`'s
 * `reasonFor` — a restriction's `call` names the callee, not a replacement argument type — so
 * the fallback is a single generic line.
 */
function reasonFor(restriction) {
  if (restriction.reason !== undefined) return restriction.reason
  return 'this argument type is restricted here'
}
