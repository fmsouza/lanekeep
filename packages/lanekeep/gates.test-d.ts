/**
 * Type-level check that a rule may declare all four pre-parse gates against the shipped
 * `index.d.ts`. Hand-written sibling of the generated `types.test-d.ts` — the gates
 * interface is a fixed template in `crates/lanekeep-types-gen`, not generated from WIT, so
 * nothing but a compile guards it.
 */

import { defineRule } from './index'

// All four fields, on purpose: `Gates` is an interface, so a field dropped from the
// rendered type turns these excess properties into compile errors. This file is the
// regression guard for the authoring surface; the extraction guard lives in
// `lanekeep-config`'s tests.
defineRule({
  id: 'local/gates',
  severity: 'error',
  query: '(identifier) @id',
  card: { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } },
  gates: {
    pathMatches: ['src/**/*.ts'],
    pathNotMatches: ['**/generated/**'],
    fileContains: ['call'],
    fileNotContains: ['skip'],
  },
})
