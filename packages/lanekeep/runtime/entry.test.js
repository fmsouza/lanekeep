/**
 * `register` and `configure`, which are the two places a rule's shape is decided.
 *
 * # Importing the module under test withholds this process's globals
 *
 * `entry.js` calls `withhold()` at its top level — that ordering is the sandbox, and
 * `host.test.js` covers what it removes. The consequence here is that `Date`, `console`,
 * `performance` and `process` are gone from `globalThis` the instant the import resolves, and
 * `node --test` needs all four to report a result. So the import is dynamic, wrapped in a save
 * and a restore, which is also why this file cannot use a static one: ESM hoists those above
 * any code that could take a copy.
 *
 * The restore is deliberately narrow — what the runner needs and nothing else — so this file
 * cannot become a way to test rule code with its authority quietly handed back.
 */

import assert from 'node:assert/strict'
import { test } from 'node:test'

const saved = {
  Date: globalThis.Date,
  console: globalThis.console,
  performance: globalThis.performance,
  process: globalThis.process,
}
const { register, rules, metadata, configure } = await import('./entry.js')
Object.assign(globalThis, saved)

/** A rule object with every field `metadata` reads. */
function rule(id, extra = {}) {
  return {
    id,
    severity: 'error',
    query: '(identifier) @it',
    card: {
      message: 'no',
      remediation: 'do the other thing',
      examples: { bad: 'a', good: 'b' },
    },
    check() {},
    ...extra,
  }
}

test('a rule object and a factory both register, and keep their order', () => {
  register([rule('local/plain'), (options) => rule('local/made', { options })])

  assert.deepEqual(rules(), ['local/plain', 'local/made'])
  assert.equal(metadata(0).id, 'local/plain')
  assert.equal(metadata(1).id, 'local/made')
})

test('a factory is applied to its options, and a rule object is used as it comes', () => {
  // The table is shared across this file's tests, so the indices continue from above rather
  // than restarting. Deliberate: `register` is called once per component at build time, and a
  // reset would be a code path no component has.
  configure(1, JSON.stringify({ limit: 7 }))
  assert.equal(metadata(1).id, 'local/made')

  // `null` is what the host sends for a rule named with no options, and it must reach a factory
  // as `undefined` rather than as `null` — a rule reading `options?.limit` works either way, one
  // reading `options.limit ?? 1` does not.
  configure(0, 'null')
})

test('a rule that is not a factory refuses options rather than ignoring them', () => {
  assert.throws(
    () => configure(0, JSON.stringify({ limit: 7 })),
    (thrown) =>
      typeof thrown === 'string' &&
      thrown.includes('local/plain') &&
      thrown.includes('takes no options'),
  )
})

test('a factory whose id moves with its options is refused', () => {
  register([(options) => rule(options ? 'local/configured' : 'local/bare')])

  assert.throws(
    () => configure(2, JSON.stringify({})),
    (thrown) => typeof thrown === 'string' && thrown.includes('cannot depend on its options'),
  )
})

test('a rule with no id is refused, by index, because there is nothing else to call it', () => {
  assert.throws(() => register([{ severity: 'error' }]), {
    name: 'TypeError',
    message: /index 0 has no `id`/,
  })
})

test('a rule missing what `metadata` reads is refused by name and by field', () => {
  // The failure this closes: `metadata` returns a record rather than a result, so a missing
  // `card` is a `TypeError` on `undefined.message` inside a wasm call — which reaches the host
  // as `wasm trap: unreachable` with the message, the type and the stack gone. Naming the rule
  // and the field is only possible here, at build time.
  for (const [broken, expected] of [
    [{ id: 'local/x', query: 'q', card: rule('local/x').card }, /severity/],
    [{ id: 'local/x', severity: 'error', card: rule('local/x').card }, /query/],
    [{ id: 'local/x', severity: 'error', query: 'q' }, /card/],
    [
      { id: 'local/x', severity: 'error', query: 'q', card: { message: 'm' } },
      /card\.remediation/,
    ],
    [
      {
        id: 'local/x',
        severity: 'error',
        query: 'q',
        card: { message: 'm', remediation: 'r', examples: { bad: 'b' } },
      },
      /card\.examples\.good/,
    ],
  ]) {
    assert.throws(() => register([broken]), { name: 'TypeError', message: expected })
    // And the rule is named, which is the whole difference from the trap it replaces.
    assert.throws(() => register([broken]), { message: /local\/x/ })
  }
})

test('register refuses anything that is not an array', () => {
  assert.throws(() => register(rule('local/lonely')), { name: 'TypeError' })
})
