/**
 * Type-level check that a rule may declare `requires` against the shipped `index.d.ts`, and
 * that the union admits nothing else. Sibling of `gates.test-d.ts`: the `Rule` interface is a
 * fixed template in `crates/lanekeep-types-gen`, not rendered from the WIT world, so nothing
 * but a compile guards its shape.
 *
 * The negative cases matter more than the positive one here. `requires` exists to make a
 * missing analysis loud, and a union that had widened to `string` would accept every typo at
 * compile time and leave the whole diagnosis to a config-load error.
 */

import { defineRule } from './index'

const card = { message: 'no', remediation: 'do this', examples: { bad: 'a', good: 'b' } }

// Both capabilities, and both together. No rule can load with these yet — the engine refuses
// a declaration it cannot honor — but the authoring surface has to accept what it documents.
defineRule({
  id: 'local/types',
  severity: 'error',
  query: '(identifier) @id',
  card,
  requires: ['types'],
})

defineRule({
  id: 'local/both',
  severity: 'error',
  query: '(identifier) @id',
  card,
  requires: ['types', 'dataflow'],
})

// Declaring nothing, in both spellings. An empty list is not a malformed one.
defineRule({
  id: 'local/empty',
  severity: 'error',
  query: '(identifier) @id',
  card,
  requires: [],
})

defineRule({
  id: 'local/unknown',
  severity: 'error',
  query: '(identifier) @id',
  card,
  // @ts-expect-error a capability outside the union
  requires: ['speed'],
})

defineRule({
  id: 'local/scalar',
  severity: 'error',
  query: '(identifier) @id',
  card,
  // @ts-expect-error the array is the only shape; a bare string is the mistake this invites
  requires: 'types',
})
