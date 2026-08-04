import { defineRule } from 'lanekeep'
import { isNestedInPath } from '../modules/rust'

/**
 * An unordered collection in a crate whose iteration order leaves it.
 *
 * §11 sorts violations `(ruleId, file, line, column)` always, because an agent reads the
 * output twice and must not see reordering as change. §8.3 makes the same demand of the cache
 * file: "the stored bytes are a function of the entries alone — insertion order does not
 * leak", so a run over unchanged input rewrites an identical file.
 *
 * Both crates hold to that today by using `BTreeMap` and `BTreeSet` throughout, and both say
 * so in comments next to the type. A comment is not a gate; this is.
 */
const UNORDERED = ['HashMap', 'HashSet']

export default function deterministicIteration(options) {
  const scope = options?.scope ?? []

  // A rule scoped to nothing checks nothing and reports a clean run, which is the failure
  // this whole rule set exists to prevent. `allow` defaults to empty and that is safe — it
  // makes the rule check *more*. `scope` is the opposite polarity, so an empty one has to be
  // refused rather than defaulted. Loud at config load beats silent forever.
  if (scope.length === 0) {
    throw new Error(
      'local/deterministic-iteration needs a non-empty `scope`; with none it silently checks nothing',
    )
  }

  return defineRule({
    id: 'local/deterministic-iteration',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'unordered collection where iteration order reaches output',
      remediation: 'use `BTreeMap` or `BTreeSet` — the order this iterates in becomes report order or cache bytes',
      examples: {
        bad: 'let grouped: HashMap<RuleId, Vec<Violation>> = HashMap::new();',
        good: 'let grouped: BTreeMap<RuleId, Vec<Violation>> = BTreeMap::new();',
      },
    },

    query: `
      [
        (use_declaration argument: (_) @name)
        (type_identifier) @name
        (scoped_identifier path: (identifier) @name)
      ] @site
    `,

    check(ctx, m) {
      if (!scope.some((prefix: string) => ctx.filePath.startsWith(prefix))) return
      if (isNestedInPath(ctx, m.site)) return

      const text = ctx.text(m.name)
      for (const unordered of UNORDERED) {
        if (!text.includes(unordered)) continue
        ctx.report(m.site, {
          message: `\`${unordered}\` iterates in an unspecified order, and that order leaves this crate`,
        })
        return
      }
    },
  })
}
