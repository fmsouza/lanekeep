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
 * **`require` is matched on the module a type came from and on the name that module exports
 * it under.** The comparison reads the exported name and never the use-site spelling, so
 * `import { Decimal as Money } from 'decimal.js'` is accepted by a convention requiring
 * `Decimal`, and `import { Big } from 'decimal.js'` is reported: a sibling export of the
 * required module is not the required type. A default import,
 * `import Decimal from 'decimal.js'`, is accepted on the module alone — what the module
 * exports it under is the literal `default`, so comparing names there would accuse
 * conforming code.
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

  for (const [index, convention] of conventions.entries()) {
    const require = convention.require
    if (require === undefined) continue
    if (
      require === null ||
      typeof require !== 'object' ||
      typeof require.module !== 'string' ||
      typeof require.name !== 'string'
    ) {
      // A `require` is matched on `module` *and* `name`, and `name` is what the message tells
      // the reader to use — so a convention missing either has nothing to check against, and
      // running it would accuse every conforming import from that module with a message that
      // says `use undefined`. Refusing here, at load, is the loud version of that failure.
      throw new Error(
        `no-restricted-types: conventions[${index}].require needs both \`module\` and \`name\` ` +
          `as strings — got ${JSON.stringify(require)}`,
      )
    }
  }

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

    // The three binding shapes ask about the *name*, because that path resolves through
    // `declaration_of` and reads an initializer where there is no annotation — `const amount
    // = 1` types as `number`. The two member shapes cannot: a property is not a binding, so
    // the oracle has nothing to resolve and `typeOf` on the name answers `undefined`, which
    // this rule turns into silence. They capture the annotation as well and ask about that.
    //
    // `property_signature` is the node kind for an interface member, a type-alias member and
    // an inline object type alike, so one clause covers all three. A member with no `type:`
    // field — `class Order { amount = 1 }` — matches neither clause and is not a candidate.
    query: `[
      (required_parameter pattern: (identifier) @name)
      (optional_parameter pattern: (identifier) @name)
      (variable_declarator name: (identifier) @name)
      (public_field_definition name: (property_identifier) @name type: (type_annotation) @type)
      (property_signature name: (property_identifier) @name type: (type_annotation) @type)
    ]`,

    check(ctx, m) {
      const name = ctx.text(m.name)

      // `??` and never `||`. A node handle is an integer and the root's is `0`, so the
      // ordinary JavaScript spelling would discard a legitimate handle. Nothing here can
      // capture the root, and the operator is still the one that is correct for the type
      // rather than for this instance.
      const subject = m.type ?? m.name

      for (const convention of conventions) {
        const names = convention.names ?? []
        if (!names.some((pattern) => matches(pattern, name))) continue

        const type = ctx.types.typeOf(subject)

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

        // A named type. It satisfies the convention when it came from the required module
        // *and* is the required export of it — matched on `symbol.exported`, the name the
        // module exports it under, which is `Decimal` for `import { Decimal }` and for
        // `import { Decimal as Money }` alike. The alias is accepted because the comparison
        // never touches the use-site spelling, and a sibling export, `import { Big } from
        // 'decimal.js'`, is reported, which the module comparison alone used to accept.
        //
        // Both halves are load-bearing, and the shadow is not what pins the module half: a
        // local `class Decimal {}` carries no module and no `exported`, so it fails the name
        // comparison first and is reported either way. What only the module comparison
        // catches is the required name imported from the wrong module —
        // `import { Decimal } from 'big.js'` — and dropping the name comparison is the false
        // negative this rule shipped with.
        //
        // A default import is accepted on the module requirement alone, as the *fallback*
        // for a package the oracle could not read. With the declaration file readable, the
        // cross-file oracle already follows the default export to the name that file declares
        // it under, and `exported` is that name — so the ordinary comparison happens and a
        // package whose default export is `Big` no longer satisfies a convention requiring
        // `Decimal`. What is left is the unresolvable case: an uninstalled package answers the
        // literal `'default'`, which no `require.name` a convention writes will equal, and
        // demanding the name there would accuse `import Decimal from 'decimal.js'` —
        // conforming code, and the one failure this design forbids.
        //
        // A nominal type the oracle could not attribute carries no symbol at all, so it
        // cannot match and is reported: a governed value whose type cannot be established
        // is not evidence the convention is met.
        if (convention.require === undefined) continue
        const symbol = type.symbol
        const satisfied =
          symbol !== undefined &&
          symbol.module === convention.require.module &&
          (symbol.exported === convention.require.name || symbol.exported === 'default')
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
