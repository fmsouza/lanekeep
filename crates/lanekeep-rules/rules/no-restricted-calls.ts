import { defineRule } from 'lanekeep'
import { appliesTo, matches } from 'lanekeep/patterns'

/**
 * Forbid calling given callables, optionally only from given paths.
 *
 * The call-expression sibling of `no-restricted-imports`: "no `console.*` outside the
 * logging layer", "no raw `fetch` outside the API client" — each is one restriction
 * entry instead of a bespoke local rule.
 *
 * A factory rather than a rule object, on the same terms as the import sibling: the
 * restriction is the whole content of the rule.
 *
 * One rule, five grammars: `language` names them and `query` carries one entry per grammar.
 * The restriction grammar, the `from` carve-outs and the raw-text matching are identical in
 * every language — what varies is which nodes are a call and how a qualified callee is
 * spelled (`console.log`, `requests.get`, `fmt.Println`, `std::fs::read`). A rust macro is
 * not a call and cannot be restricted here.
 *
 * @example
 * ```ts
 * import noRestrictedCalls from 'lanekeep/no-restricted-calls'
 *
 * export default defineConfig({
 *   rules: [
 *     noRestrictedCalls({
 *       restrictions: [
 *         { call: 'console.*', from: ['!src/logging/**'], reason: 'route it through the logger' },
 *         { call: 'fetch',     from: ['!src/api/**'],     reason: 'use the API client' },
 *       ],
 *     }),
 *   ],
 * })
 * ```
 */
export default function noRestrictedCalls(options) {
  const restrictions = options?.restrictions ?? []

  return defineRule({
    id: 'lanekeep/no-restricted-calls',
    language: ['typescript', 'tsx', 'python', 'go', 'rust'],
    severity: 'error',

    card: {
      message: 'restricted call',
      remediation: 'call something permitted here, or move this code where it is allowed',
      examples: {
        bad: 'console.log(metrics)',
        good: 'log(metrics)',
      },
    },

    // No gates: a restriction list has no single substring every violating file
    // contains, and `fileContains` is an *and* with no *or* form — a wrong gate is
    // worse than none.
    //
    // One query per grammar it names. Python alone spells a call `call`; the callee
    // shapes are each grammar's bare and qualified forms. A rust `macro_invocation` is
    // deliberately not among them — `println!` is not a `call_expression`, and
    // restricting macros is out of scope.
    query: {
      typescript:
        '(call_expression function: [(identifier) (member_expression)] @callee) @call',
      tsx: '(call_expression function: [(identifier) (member_expression)] @callee) @call',
      python: '(call function: [(identifier) (attribute)] @callee) @call',
      go: '(call_expression function: [(identifier) (selector_expression)] @callee) @call',
      rust: '(call_expression function: [(identifier) (scoped_identifier) (field_expression)] @callee) @call',
    },

    check(ctx, m) {
      // The callee as written, normalized once: whitespace stripped (`console\n  .log`
      // reads as `console.log`) and `?.` folded to `.` so `console?.log` matches
      // `console.*`. Matching the raw text rather than a resolved name is deliberate,
      // on the same terms as the import sibling.
      const callee = ctx
        .text(m.callee)
        .replace(/\s+/g, '')
        .replace(/\?\./g, '.')
      const file = ctx.filePath

      for (const restriction of restrictions) {
        if (!matches(restriction.call, callee)) continue
        if (!appliesTo(restriction.from, file)) continue

        const reason = restriction.reason
          ? `${restriction.reason}`
          : 'this call is not allowed here'
        ctx.report(m.call, `calling '${callee}' is restricted — ${reason}`)
        return
      }
    },
  })
}
