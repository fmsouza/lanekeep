/**
 * Type-level check that `ctx.structureFingerprint` is callable on the shipped
 * `index.d.ts` and returns the `{ hash, nodes }` shape, and that the return is
 * `undefined`-able (a dead handle). Hand-written sibling of the generated
 * `types.test-d.ts` — a dropped or renamed member of the return shape turns the
 * property reads below into compile errors, which is the guard this file exists to be.
 */

import { defineRule } from './index'

defineRule({
  id: 'local/structure-fingerprint',
  severity: 'error',
  query: '(function_declaration) @fn',
  card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },
  check(ctx, match) {
    // A capture is `Node | undefined` — a capture that did not participate is absent.
    if (match.fn === undefined) return
    const fp = ctx.structureFingerprint(match.fn)
    if (fp === undefined) return
    // `nodes` is the thresholding input; `hash` the grouping key.
    if (fp.nodes < 5) return
    fp.hash.toUpperCase()
  },
})
