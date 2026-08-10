// The glue module, tested where it is cheap to test it.
//
// `host.js` has two halves and they need different kinds of evidence. The withholding is only
// meaningful inside a component — the globals it deletes are ones Node has and QuickJS never
// had — so `crates/lanekeep-wasm/tests/js_globals.rs` is where that claim is made, against a
// built artifact, with the probe list derived from `crates/lanekeep-js/src/sandbox.rs`. What is
// left is the `ctx` translation: pure data-shape work between the world's `option`s, `result`s
// and capture lists and the API `packages/lanekeep/index.d.ts` publishes. That is what this
// file covers, against `crates/lanekeep-js/src/host.rs`, which is the specification both
// engines follow.
//
// `entry.js` is deliberately not imported here. It calls `withhold()` at its top level — that
// ordering is the whole point of it — and Node's test runner needs the `process`, `console` and
// `setTimeout` that call would take away. It is covered end to end by the component fixture
// instead, which is the only place the seven world exports mean anything.

import { strict as assert } from 'node:assert'
import test from 'node:test'

import { WITHHELD, buildCheckContext, buildReduceContext, readReportOptions, toMatch, withhold } from './host.js'

/**
 * A stand-in for the world's `check-context`, recording what was called on it.
 *
 * Every member is a function, because that is what a WIT resource's methods are; the
 * translation to properties is what `buildCheckContext` does and what these tests are about.
 */
function stubContext(overrides = {}) {
  const calls = []
  const record =
    (name, result) =>
    (...args) => {
      calls.push(`${name}(${args.map((a) => JSON.stringify(a)).join(',')})`)
      return typeof result === 'function' ? result(...args) : result
    }

  return {
    calls,
    ctx: {
      filePath: record('filePath', 'src/a.ts'),
      fileText: record('fileText', 'const x = 1;\n'),
      root: record('root', 0),
      today: record('today', undefined),
      kind: record('kind', 'program'),
      text: record('text', 'const x = 1;\n'),
      isNamed: record('isNamed', true),
      line: record('line', 1),
      column: record('column', 1),
      parent: record('parent', undefined),
      children: record('children', [1, 2]),
      namedChildren: record('namedChildren', [1]),
      ancestors: record('ancestors', []),
      resolvesToImport: record('resolvesToImport', false),
      isImportedFrom: record('isImportedFrom', false),
      bindingKind: record('bindingKind', undefined),
      isShadowed: record('isShadowed', false),
      querySubtree: record('querySubtree', [[{ name: 'd', node: 3 }]]),
      closestAncestor: record('closestAncestor', undefined),
      readFile: record('readFile', undefined),
      fileExists: record('fileExists', false),
      emitFact: record('emitFact', undefined),
      loc: record('loc', { file: 'src/a.ts', line: 1, column: 1 }),
      report: record('report', undefined),
      ...overrides,
    },
  }
}

// --- the withheld list ------------------------------------------------------------------------

test('withhold deletes every name it lists, including the dotted ones', () => {
  const target = { Date: class {}, fetch: () => {}, Math: { random: () => 0.5, max: Math.max } }

  const survivors = withhold(target)

  assert.deepEqual(survivors, [])
  assert.equal('Date' in target, false)
  assert.equal('fetch' in target, false)
  assert.equal('random' in target.Math, false)
  // A dotted deletion takes the property and not the object it hangs off.
  assert.equal(typeof target.Math.max, 'function')
})

test('withhold reports a name it could not delete rather than claiming success', () => {
  // The one case where deleting silently fails: a non-configurable property. A sandbox with a
  // hole in it must not look like a sandbox without one.
  const target = {}
  Object.defineProperty(target, 'Date', { value: class {}, configurable: false })

  assert.deepEqual(withhold(target), ['Date'])
})

test('withhold is a no-op on a name that is not there', () => {
  // Most of the list is already absent from a component; `delete` on an absent property
  // succeeds, and a missing owner is skipped rather than thrown on.
  assert.deepEqual(withhold({}), [])
})

test('the withheld list has no duplicates', () => {
  // A duplicate is harmless to run and hides a merge that went wrong, and the Rust test
  // compares sets — so it would never be the thing that goes red.
  assert.equal(new Set(WITHHELD).size, WITHHELD.length)
  for (const name of WITHHELD) {
    assert.equal(typeof name, 'string')
    assert.ok(name.length > 0)
  }
})

// --- the check context ------------------------------------------------------------------------

test('filePath, fileText and root are properties over functions', () => {
  const { ctx } = stubContext()
  const built = buildCheckContext(ctx)

  assert.equal(built.filePath, 'src/a.ts')
  assert.equal(built.fileText, 'const x = 1;\n')
  assert.equal(built.root, 0)
})

test('a property read twice crosses the boundary once, and unread crosses none', () => {
  // `check` runs once per query match, so a rule that never looks at the file's text must not
  // pay for copying it across — that is the invariant about JavaScript executing proportional
  // to matches, seen from the other side of the boundary.
  const { calls, ctx } = stubContext()
  const built = buildCheckContext(ctx)

  assert.equal(built.fileText, 'const x = 1;\n')
  assert.equal(built.fileText, 'const x = 1;\n')

  assert.equal(calls.filter((call) => call.startsWith('fileText')).length, 1)
  assert.equal(calls.filter((call) => call.startsWith('filePath')).length, 0)

  // And the root, whose handle is 0 — the value that makes `if (!cached)` the wrong test and
  // would leave this one crossing on every read while looking memoized.
  assert.equal(built.root, 0)
  assert.equal(built.root, 0)
  assert.equal(calls.filter((call) => call.startsWith('root')).length, 1)
})

test('today is read through, so the host can observe that it was read', () => {
  // A getter and not a value, exactly as `host.rs` installs it: the cache has to know which
  // files depend on the date, and a plain value would be indistinguishable from an unread one.
  const { calls, ctx } = stubContext({ today: () => '2026-08-10' })
  const built = buildCheckContext(ctx)

  assert.equal(built.today, '2026-08-10')
  assert.equal(built.today, '2026-08-10')
  assert.equal(calls.filter((call) => call.startsWith('today')).length, 0)
})

test('a match reaches a rule as captures keyed by name', () => {
  assert.deepEqual(toMatch([{ name: 'source', node: 7 }, { name: 'stmt', node: 2 }]), {
    source: 7,
    stmt: 2,
  })
  // A capture that did not participate is absent from the list and so absent from the object,
  // which is what makes the published type's values optional.
  assert.deepEqual(toMatch([]), {})
})

test('querySubtree hands back captures, and closestAncestor hands back undefined', () => {
  const { ctx } = stubContext()
  const built = buildCheckContext(ctx)

  assert.deepEqual(built.querySubtree(0, '(x) @d'), [{ d: 3 }])
  // `option<match>` rather than an empty list: an empty object is truthy, so a "nothing
  // matched" value shaped like a match takes the wrong branch under `if (!...)`.
  assert.equal(built.closestAncestor(0, '(x) @p'), undefined)
})

test('report takes a message or an options object', () => {
  assert.deepEqual(readReportOptions(undefined), [undefined, undefined])
  assert.deepEqual(readReportOptions('why'), ['why', undefined])
  assert.deepEqual(readReportOptions({ message: 'why' }), ['why', undefined])
  assert.throws(() => readReportOptions(7), /message string or an options object/)
})

test('an omitted `safe` stays omitted, so the host decides what it means', () => {
  // The two mistakes are not symmetric: a suggestion that should have been safe costs a manual
  // edit, and a fix that should have been a suggestion rewrites someone's code. Sending `false`
  // from here would make this file the place that decided.
  const [, unspecified] = readReportOptions({ fix: { node: 3, text: 'x' } })
  assert.equal(unspecified.safe, undefined)

  const [, claimed] = readReportOptions({ fix: { node: 3, text: 'x', safe: true } })
  assert.equal(claimed.safe, true)
})

test('emitFact refuses what could never be read back', () => {
  const { ctx } = stubContext()
  const built = buildCheckContext(ctx)

  assert.throws(() => built.emitFact('not an object'), /expects an object/)
  assert.throws(() => built.emitFact({}), /non-empty string `kind`/)
  assert.throws(() => built.emitFact({ kind: '' }), /non-empty string `kind`/)
  // `JSON.stringify` is the definition of serializable, so a cycle throws from inside it and
  // surfaces as the rule's own error.
  const cyclic = { kind: 'x' }
  cyclic.self = cyclic
  assert.throws(() => built.emitFact(cyclic))
})

test('emitFact sends the kind and the payload as the world declares them', () => {
  const { calls, ctx } = stubContext()
  buildCheckContext(ctx).emitFact({ kind: 'export', symbol: 'parse' })

  assert.deepEqual(calls, ['emitFact("export","{\\"kind\\":\\"export\\",\\"symbol\\":\\"parse\\"}")'])
})

test('a refused read throws in lanekeep_core::ReadError\'s own words', () => {
  // A refusal throws and an absence returns undefined, and the distinction is the point: "not
  // there" is an ordinary answer a rule handles, and "you tried to leave the project" is a bug
  // in the rule. The prose is `crates/lanekeep-core/src/files.rs`'s, so a rule author sees one
  // refusal rather than one per engine.
  const refuse = (tag) => () => {
    throw { payload: { tag, val: '../secrets' } }
  }

  const escapes = buildCheckContext(stubContext({ readFile: refuse('escapes-root') }).ctx)
  assert.throws(() => escapes.readFile('../secrets'), /it resolves outside the project root/)

  const absolute = buildCheckContext(stubContext({ readFile: refuse('absolute') }).ctx)
  assert.throws(() => absolute.readFile('../secrets'), /reads are relative to the project root/)

  const binary = buildCheckContext(stubContext({ readFile: refuse('not-text') }).ctx)
  assert.throws(() => binary.readFile('../secrets'), /is not valid UTF-8/)
})

test('a read that found nothing is an ordinary undefined', () => {
  const built = buildCheckContext(stubContext().ctx)
  assert.equal(built.readFile('missing.ts'), undefined)
  assert.equal(built.fileExists('missing.ts'), false)
})

// --- the reduce context -----------------------------------------------------------------------

test('a fact reaches the reduce phase merged with the file it came from', () => {
  const built = buildReduceContext({
    files: () => ['a.ts', 'b.ts'],
    facts: () => [
      // Claiming a file it did not come from. The host's value has to win, or a rule could
      // misattribute a violation to a file it never saw.
      { kind: 'export', file: 'a.ts', data: '{"kind":"export","file":"lies.ts","symbol":"parse"}' },
    ],
    report: () => {},
  })

  assert.deepEqual(built.facts(), [{ kind: 'export', file: 'a.ts', symbol: 'parse' }])
  assert.deepEqual(built.files, ['a.ts', 'b.ts'])
})

test('the reduce phase reports at a location and refuses anything else', () => {
  const seen = []
  const built = buildReduceContext({
    files: () => [],
    facts: () => [],
    report: (at, message) => seen.push([at, message]),
  })

  built.report({ file: 'a.ts', line: 3, column: 4 }, 'cycle')
  assert.deepEqual(seen, [[{ file: 'a.ts', line: 3, column: 4 }, 'cycle']])

  // There is no parse tree in this phase, so there are no nodes to report at — a rule handing
  // over a node is making a category error and should hear about it.
  assert.throws(() => built.report(7, 'cycle'), /there are no nodes to report at/)
})
