/**
 * Type-level check that both `report` signatures say what both engines do: a fix on the
 * per-file context compiles, the `{ message }` object form compiles on both, and a fix
 * offered in a reduce report is a compile error, as is a reduce location missing its `line`
 * or its `column` — the published rendering of the refusals both hosts make
 * (`crates/lanekeep-js/tests/report_parity.rs` holds the run-time half to the same words).
 *
 * Nothing here runs. `defineRule` is an identity function; a `.test-d.ts` file exists to be
 * compiled, and `tsc --noEmit -p packages/lanekeep` is the check.
 *
 * The negative cases are what keep `index.d.ts`'s narrowing from being regenerated away: the
 * byte-exact test (`crates/lanekeep-types-gen/tests/generated.rs`) is satisfied again the
 * moment the file is re-blessed, so it cannot notice a generator regression — a signature
 * that accepted a `fix` again would pass it, while every fix-offering rule stopped compiling,
 * and a `ReduceLocation` whose `line` or `column` went optional again would pass it, while a
 * rule omitting either compiled and then threw at run time.
 */

import { defineRule } from './index'
import type { Node } from './index'

defineRule({
  id: 'local/report-per-file',
  severity: 'error',
  query: '(identifier) @id',
  card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },
  check(ctx, match) {
    if (match.id === undefined) return

    // A fix needs a node, and the per-file phase has one: the capture that found the
    // violation. Both spellings of the second argument are ordinary.
    ctx.report(match.id, 'the card says less than this')
    ctx.report(match.id, {
      message: 'specific',
      fix: { node: match.id, text: 'let x = 1', safe: true },
    })
  },
})

defineRule({
  id: 'local/report-reduce',
  severity: 'error',
  query: '(program) @p',
  card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },
  reduce(ctx) {
    // The bare string and the options object are both ordinary calls — the `{ message }`
    // form is what both engines accept, and the one narrowing may not take away.
    ctx.report({ file: ctx.files[0], line: 1, column: 1 }, 'cycle')
    ctx.report({ file: ctx.files[0], line: 1, column: 1 }, { message: 'cycle' })

    // A fix offered through an object built before the call. A fresh literal with a stray
    // `fix` would already fail the excess-property check; `fix?: never` is what makes this
    // fail too, which is the shape an extracted helper or a reused options object takes.
    const report: { message: string; fix: { node: Node; text: string; safe: boolean } } = {
      message: 'cycle',
      fix: { node: 1 as Node, text: 'let x = 1', safe: true },
    }
    // @ts-expect-error — a fix is a compile error here: the reduce phase has no parse tree,
    // so `node` has nothing to name, and both hosts throw on a supplied fix.
    ctx.report({ file: ctx.files[0], line: 2, column: 1 }, report)
  },
})

defineRule({
  id: 'local/report-reduce-fresh-literal',
  severity: 'error',
  query: '(program) @p',
  card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },
  reduce(ctx) {
    // @ts-expect-error — the fresh-literal spelling of the same refusal: the object literal carries a `fix`, and `ReduceReportOptions` has no such field to offer it to.
    ctx.report({ file: ctx.files[0], line: 3, column: 1 }, { message: 'cycle', fix: { node: 1 as Node, text: 'let x = 1', safe: true } })
  },
})

defineRule({
  id: 'local/report-reduce-location',
  severity: 'error',
  query: '(program) @p',
  card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },
  reduce(ctx) {
    // A finding about a file as a whole — a key missing from a locale file — says 1:1 itself.
    ctx.report({ file: ctx.files[0], line: 1, column: 1 }, 'missing key')

    // Each half on its own, because both hosts refuse each on its own: a line with no column
    // is the report a rule most plausibly writes, and it throws exactly as one naming neither.
    // @ts-expect-error — a line with no column: both hosts throw on it, so the type refuses it too.
    ctx.report({ file: ctx.files[0], line: 3 }, 'missing key')
    // @ts-expect-error — a column with no line, the other half, refused on the same terms.
    ctx.report({ file: ctx.files[0], column: 1 }, 'missing key')
    // @ts-expect-error — a file alone is not a location either host accepts.
    ctx.report({ file: ctx.files[0] }, 'missing key')
  },
})