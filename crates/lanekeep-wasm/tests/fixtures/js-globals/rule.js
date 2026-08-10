// A JavaScript rule component, built the way the shipped built-ins component will be, for
// the one property that cannot be tested any other way: what a rule can still reach once
// `componentize-js` has put it inside StarlingMonkey.
//
// `crates/lanekeep-wasm/tests/js_globals.rs` drives it and documents why. In short: a
// component built with `--disable all` has exactly one import and no clock at any level, and
// a rule inside it could still call `Date.now()` and get the instant the component was built.
// `packages/lanekeep/runtime/host.js` is the repair; this is what proves it took.
//
// The runtime import is written first on purpose. ES modules evaluate depth-first in source
// order, so this is what makes `withhold()` run before any rule module's own body — see
// `entry.js`. A generated entry emits exactly this shape.
import { register } from '../../../../../packages/lanekeep/runtime/entry.js'

export {
  rules,
  metadata,
  configure,
  hasCheck,
  hasReduce,
  check,
  reduce,
} from '../../../../../packages/lanekeep/runtime/entry.js'

/**
 * A card, since every rule needs one and none of these rules is about its card.
 *
 * @param {string} what
 */
const card = (what) => ({
  message: what,
  remediation: `nothing — \`${what}\` is a fixture`,
  examples: { bad: 'bad', good: 'good' },
})

/**
 * Evaluate the expression the capture is named after, and report what came back.
 *
 * The probe arrives as a *capture name* rather than as an option because the host chooses it
 * per call, and `configure` happens once. `js_globals.rs` derives the list from what
 * `crates/lanekeep-js/src/sandbox.rs` withholds under QuickJS, so a name added there is
 * probed here without this file being touched — which is the whole point of putting the
 * expression on the Rust side.
 *
 * Reporting on success rather than returning quietly is what puts the *frozen value* in the
 * failing test's output. `Date.now` reachable is not interesting; `Date.now` reachable and
 * equal to the same number in every process is the finding.
 */
const reach = {
  id: 'probe/reach',
  severity: 'error',
  card: card('reach'),
  query: '(program) @p',

  check(ctx, m) {
    const probe = Object.keys(m)[0]
    // Indirect `eval`, so the expression is evaluated in global scope rather than in this
    // closure — a bare `Date` has to be a global reference for its absence to be the
    // `ReferenceError` a rule author would see.
    const value = (0, eval)(probe)
    ctx.report(ctx.root, `${probe} evaluated to ${describe(value)}`)
  },
}

/**
 * Exercise the translation `buildCheckContext` performs, and report what it saw.
 *
 * A factory rather than a rule object, so that `configure` is exercised by something: the
 * note it takes appears in the reported line, so a configuration that never arrived is
 * visible rather than assumed.
 *
 * The binding and tracked-read methods are deliberately not called. Both need host state this
 * fixture's harness does not build, and `read-file` against a context with no file access
 * *traps* — which would poison the store for every later probe and report the wrong thing.
 * The four migrated rules exercise them against a real engine.
 */
const context = (options) => ({
  id: 'probe/context',
  severity: 'error',
  language: ['typescript', 'tsx'],
  card: card('context'),
  gates: { fileContains: ['const'] },
  query: '(program) @p',
  timeout: 250,

  check(ctx) {
    const root = ctx.root
    const matches = ctx.querySubtree(root, '(lexical_declaration) @d')
    const parts = [
      `note=${options?.note}`,
      `filePath=${ctx.filePath}`,
      `fileText=${JSON.stringify(ctx.fileText)}`,
      `root=${root}`,
      `kind=${ctx.kind(root)}`,
      `text=${JSON.stringify(ctx.text(root))}`,
      `isNamed=${ctx.isNamed(root)}`,
      `line=${ctx.line(root)}`,
      `column=${ctx.column(root)}`,
      `parent=${ctx.parent(root)}`,
      `children=${ctx.children(root).length}`,
      `namedChildren=${ctx.namedChildren(root).length}`,
      `ancestors=${ctx.ancestors(root).length}`,
      `bindingKind=${ctx.bindingKind(root)}`,
      `isShadowed=${ctx.isShadowed(root)}`,
      `subtree=${matches.length}:${Object.keys(matches[0] ?? {}).join('+')}`,
      `closest=${ctx.closestAncestor(root, '(program) @p')}`,
      `loc=${JSON.stringify(ctx.loc(root))}`,
      `today=${ctx.today}`,
    ]
    ctx.emitFact({ kind: 'probe', seen: parts.length })
    ctx.report(root, parts.join(' ; '))
  },
})

/** Exercise the translation `buildReduceContext` performs. Check-less, on purpose. */
const cross = {
  id: 'probe/cross',
  severity: 'warn',
  card: card('cross'),
  query: '(program) @p',

  reduce(ctx) {
    const all = ctx.facts()
    const narrowed = ctx.facts('export')
    ctx.report(
      { file: ctx.files[0], line: 1, column: 1 },
      `files=${ctx.files.join('+')} ; all=${JSON.stringify(all)} ; export=${JSON.stringify(narrowed)}`,
    )
  },
}

/** A thrown error, so the graceful-failure channel carries a stack rather than a trap. */
const thrower = {
  id: 'probe/throw',
  severity: 'error',
  card: card('throw'),
  query: '(program) @p',

  check() {
    inner()
  },
}

function inner() {
  throw new Error('a rule that failed on purpose')
}

/**
 * A value, in a form a test can assert on.
 *
 * `typeof` first, because "reachable" is the question and the value is the evidence.
 *
 * @param {unknown} value
 */
function describe(value) {
  if (typeof value === 'function') return 'function'
  if (value === undefined) return 'undefined'
  if (value === null) return 'null'
  return `${typeof value}:${String(value)}`
}

register([reach, context, cross, thrower])
