/**
 * The glue between a WebAssembly component's host imports and the API a rule is written
 * against — and the repair for what `componentize-js` leaves lying around.
 *
 * Two jobs, and the second is the reason this file exists at all.
 *
 * # 1. The ambient globals are present, and silently wrong
 *
 * A component built with `jco componentize --disable all` has exactly one import,
 * `lanekeep:host/types@0.1.0`. No clock, no randomness, no filesystem, at any level — that was
 * measured, at the component's import list and at all 29 of its core-module imports. And yet a
 * rule running inside it observed, on 2026-08-10:
 *
 * ```text
 * Date.now=1786352655014 ; Math.random=0.48401551228016615 ;
 * newDate=2026-08-10T09:04:15.014Z ; fetch=function ; setTimeout=function
 * ```
 *
 * Byte-identical across two calls in one process and across two processes minutes apart.
 * `Date.now()` is the instant the component was *built*, frozen into the `wizer` memory image;
 * `Math.random()` is a constant. Determinism survives — the component's bytes are a
 * `ruleset_hash` input, so even the frozen timestamp is part of the cache key — but the sandbox
 * invariant does not. `AGENTS.md` says these are *absent*, "not restricted — they are absent
 * from the context", and `crates/lanekeep-js/src/sandbox.rs` delivers exactly that under
 * QuickJS. Present-and-frozen is worse than either absence or failure: a rule author calling
 * `Date.now()` gets a stale build timestamp with nothing to say so.
 *
 * So {@link WITHHELD} is deleted before any rule can reach it. Deleting is enough, for the
 * reason `sandbox.rs` gives for `delete Math.random`: a rule may define its own, but whatever
 * it writes is its own code and therefore deterministic. What it cannot do is reach the
 * engine's original, because the only reference to it is gone.
 *
 * **The list is data, not prose, because a test reads it.**
 * `crates/lanekeep-wasm/tests/js_globals.rs` extracts what `sandbox.rs` withholds under QuickJS
 * and holds this array to it. Two hand-maintained lists that silently disagree is the failure
 * `crates/lanekeep-js/tests/host_types.rs` exists to prevent for the published types; this is
 * the same problem one layer down.
 *
 * # 2. Assembling the `ctx` a rule is written against
 *
 * The world hands a guest a `check-context` resource whose members are all functions —
 * `ctx.filePath()`, `ctx.kind(n)` — returning WIT `option`s and `result`s. A rule is written
 * against `packages/lanekeep/index.d.ts`, where `filePath` is a property, an absent value is
 * `undefined`, and a refused read throws. {@link buildCheckContext} and
 * {@link buildReduceContext} are that translation, and `crates/lanekeep-js/src/host.rs` is the
 * specification they follow: where the two disagree, it is right and this is wrong.
 */

/**
 * Every global withheld from rule code, in the spelling used to delete it.
 *
 * A dotted name is a property of the object it names — `Math.random` is not a global, and
 * `sandbox.rs` deletes it after the fact for exactly that reason: the base objects are not
 * optional in QuickJS, so the allowlist cannot reach it.
 *
 * Most of these are already absent from a `componentize-js` component and deleting them is a
 * no-op. They are here anyway: one list holds under both engines, and a name that arrives in a
 * future StarlingMonkey is then withheld the day it appears rather than the day somebody
 * notices.
 */
export const WITHHELD = [
  // Filesystem and process. None of these exist in a component today; all of them exist in
  // Node, which is where a rule that has not been componentized yet also has to be wrong.
  'fs',
  'process',
  'require',
  'child_process',
  'module',
  '__dirname',
  'Deno',
  'Bun',

  // Network. `fetch` is the capability; `Request`, `Response` and `Headers` are the fetch
  // API's own constructors and go with it. `Blob`, `File`, `FormData`, `URL` and
  // `URLSearchParams` deliberately stay — they are general data types that carry no authority
  // once `fetch` is gone, and deleting a platform a rule might legitimately use is not what
  // the invariant asks for.
  'fetch',
  'XMLHttpRequest',
  'WebSocket',
  'navigator',
  'Request',
  'Response',
  'Headers',

  // Timers. A scheduling primitive is a way to observe wall-clock time, and there is nothing
  // for one to schedule in a synchronous single-pass engine. The `clear*` half goes with the
  // `set*` half: leaving one of a pair reads as an oversight even though it is inert.
  'setTimeout',
  'setInterval',
  'setImmediate',
  'requestAnimationFrame',
  'clearTimeout',
  'clearInterval',

  // The clock. `Performance` beside `performance`, because the instance is not the only way
  // in: `Performance.prototype.now` is the method, and the whole point of `sandbox.rs`'s note
  // about `performance.now()` is that it survives a reviewer thinking "we removed Date, so
  // there is no clock". One level further in, the constructor survives removing the instance.
  'Date',
  'performance',
  'Performance',

  // Randomness, on the same footing: `crypto` is an instance of `Crypto`, and
  // `Crypto.prototype.getRandomValues` is the entropy source it fronts.
  'Math.random',
  'crypto',
  'Crypto',
  'SubtleCrypto',
  'CryptoKey',

  // Garbage-collection timing, which is nondeterminism that does not look like a clock and so
  // tends to be overlooked.
  'WeakRef',
  'FinalizationRegistry',

  // Shared memory. Useless without threads, and removed to keep the surface small rather than
  // because a concrete attack is known.
  'SharedArrayBuffer',
  'Atomics',

  // Ambient I/O and the environment. `console` writes to a stdio that `--disable all` removed,
  // so it is an output channel that goes nowhere; QuickJS has none either, and a rule that
  // logs should fail the same way in both engines rather than in one. `location` and its
  // `self`/`WorkerLocation` spellings describe a host environment a rule has no business
  // observing.
  'console',
  'location',
  'self',
  'WorkerLocation',
]

/**
 * Delete every name in {@link WITHHELD} from `target`.
 *
 * Idempotent, and safe on a name that is not there: `delete` on an absent property succeeds.
 *
 * **Not called at this module's top level, deliberately.** Importing this file under Node — as
 * `host.test.js` does — would otherwise delete the test runner's own `Date` and `setTimeout`
 * out from under it. `entry.js` calls it at *its* top level instead, which is the module a
 * component's generated entry imports first.
 *
 * @param {object} target The global object to strip. Defaults to `globalThis`.
 * @returns {string[]} Names that survived the attempt, which is empty on every engine seen so
 *   far. A non-configurable global cannot be deleted, and a caller that assumed otherwise
 *   should hear about it rather than ship a sandbox with a hole in it.
 */
export function withhold(target = globalThis) {
  const survivors = []

  for (const name of WITHHELD) {
    const path = name.split('.')
    const leaf = path.pop()

    let owner = target
    for (const step of path) {
      owner = owner?.[step]
    }
    // An owner that is not there is not a survivor: `Math.random` cannot be reached if `Math`
    // has already gone, which is the one way this loop's order could matter.
    if (owner === undefined || owner === null) continue

    try {
      delete owner[leaf]
    } catch {
      // A module is always strict, and strict-mode `delete` on a non-configurable property
      // *throws* rather than returning false. Swallowing it here is what lets the loop finish
      // and report every survivor at once — a build that failed on the first one would fix
      // them one rebuild at a time, and each rebuild is a minute.
    }
    if (leaf in owner) survivors.push(name)
  }

  return survivors
}

/**
 * The `ctx` a per-file handler is written against, over the world's `check-context`.
 *
 * Built once per `check` call, because the resource is *borrowed* for the call and the handle
 * is dead the moment it returns. `filePath`, `fileText` and `root` are properties in the
 * published API and functions across the boundary, so they are memoized getters: a rule that
 * reads `ctx.fileText` twice pays for one crossing, and a rule that never reads it pays for
 * none. That second half is the one that matters — `check` runs once per query match, and
 * copying a file's whole text across the boundary per match would be the cost the "JavaScript
 * executes proportional to matches" invariant exists to avoid paying somewhere else.
 *
 * @param {object} ctx The `check-context` resource the world lent this call.
 * @returns {object} A `RuleContext`, as `packages/lanekeep/index.d.ts` declares it.
 */
export function buildCheckContext(ctx) {
  let filePath
  let fileText
  let root

  return {
    get filePath() {
      if (filePath === undefined) filePath = ctx.filePath()
      return filePath
    },
    get fileText() {
      if (fileText === undefined) fileText = ctx.fileText()
      return fileText
    },
    // The root's handle is 0, so `=== undefined` is the only test that works here. `!root` is
    // the ordinary JavaScript spelling and it would re-cross the boundary on every read — the
    // same shape as the bug that discards the root node, in a place where it only costs time.
    get root() {
      if (root === undefined) root = ctx.root()
      return root
    },
    // A property backed by a getter, as in `host.rs`, so that *reading the date* can be
    // observed by the host: the cache has to know which files depend on it, and a plain value
    // would be indistinguishable from an unread one. `undefined` when the rule is not
    // permitted to observe it, which is the same value QuickJS produces by not defining the
    // property at all.
    get today() {
      return ctx.today()
    },

    kind: (node) => ctx.kind(node),
    text: (node) => ctx.text(node),
    isNamed: (node) => ctx.isNamed(node),
    line: (node) => ctx.line(node),
    column: (node) => ctx.column(node),

    parent: (node) => ctx.parent(node),
    children: (node) => ctx.children(node),
    namedChildren: (node) => ctx.namedChildren(node),
    ancestors: (node) => ctx.ancestors(node),

    resolvesToImport: (node, module, name) => ctx.resolvesToImport(node, module, name),
    isImportedFrom: (node, pattern) => ctx.isImportedFrom(node, pattern),
    bindingKind: (node) => ctx.bindingKind(node),
    isShadowed: (node) => ctx.isShadowed(node),

    querySubtree: (node, query) => ctx.querySubtree(node, query).map(toMatch),
    closestAncestor: (node, query) => {
      const found = ctx.closestAncestor(node, query)
      // `option<match>` rather than an empty list, because an empty object is truthy and a
      // "nothing matched" value shaped like a match takes the wrong branch under `if (!...)`.
      return found === undefined || found === null ? undefined : toMatch(found)
    },

    readFile: (path) => unwrapRead(() => ctx.readFile(path), path),
    fileExists: (path) => unwrapRead(() => ctx.fileExists(path), path),

    emitFact: (fact) => {
      if (fact === null || typeof fact !== 'object') {
        throw new TypeError('ctx.emitFact expects an object')
      }
      // `kind` is what `ctx.facts('export')` filters on. A fact without one could never be
      // retrieved, so emitting it is always a mistake — and a silent one, since the rule would
      // look like it was working right up until `reduce` found nothing.
      const kind = fact.kind
      if (typeof kind !== 'string' || kind.length === 0) {
        throw new TypeError(
          'ctx.emitFact requires a non-empty string `kind` — it is what ctx.facts(kind) ' +
            'selects on, so a fact without one can never be read back',
        )
      }
      // `JSON.stringify` is the definition of serializable here rather than a check alongside
      // it, so what a rule can emit is exactly what a cache entry can hold. A cycle throws
      // from inside stringify and surfaces as the rule's error.
      const data = JSON.stringify(fact)
      if (data === undefined) {
        throw new TypeError(
          'ctx.emitFact could not serialize this fact — facts are cached, so they have to ' +
            'survive JSON',
        )
      }
      ctx.emitFact(kind, data)
    },

    loc: (node) => ctx.loc(node),

    report: (at, options) => {
      const [message, fix] = readReportOptions(options)
      ctx.report(at, message, fix)
    },
  }
}

/**
 * The `ctx` a cross-file handler is written against, over the world's `reduce-context`.
 *
 * Deliberately smaller: no file, no tree, no text. The reduce phase consumes facts and the
 * file list and nothing else, and here that is structural — there is no method to decline.
 *
 * @param {object} ctx The `reduce-context` resource the world lent this call.
 * @returns {object} A `ReduceContext`, as `packages/lanekeep/index.d.ts` declares it.
 */
export function buildReduceContext(ctx) {
  let files

  return {
    get files() {
      if (files === undefined) files = ctx.files()
      return files
    },

    facts: (kind) =>
      ctx.facts(kind).map((fact) => ({
        ...JSON.parse(fact.data),
        // Last, so it overrides a `file` the rule put in the payload itself. A rule cannot
        // misattribute a violation to a file it did not come from — `host.rs`'s `merge_file`
        // relies on the same last-key-wins rule, in JSON rather than in a spread.
        file: fact.file,
      })),

    report: (at, options) => {
      if (at === null || typeof at !== 'object') {
        throw new TypeError(
          'ctx.report in a reduce phase expects { file, line, column } — there is no parse ' +
            'tree here, so there are no nodes to report at',
        )
      }
      const [message] = readReportOptions(options)
      ctx.report({ file: at.file, line: at.line, column: at.column }, message)
    },
  }
}

/**
 * One query match, as a rule reads it: captures keyed by name, without the `@`.
 *
 * The world carries a match as a list of `{ name, node }` because a query's capture names are
 * declared by the rule and cannot be a WIT record's statically named fields. A capture that
 * did not participate is simply absent from the list, and so absent from the object.
 *
 * @param {{name: string, node: number}[]} entries
 * @returns {Record<string, number>}
 */
export function toMatch(entries) {
  const match = {}
  for (const entry of entries) {
    match[entry.name] = entry.node
  }
  return match
}

/**
 * The `(message, fix)` pair behind `report`'s second argument.
 *
 * A union rather than two functions, because `ctx.report(node, 'why')` is the overwhelmingly
 * common call and should stay the short one.
 *
 * @param {string | {message?: string, fix?: object} | undefined} options
 * @returns {[string | undefined, object | undefined]}
 */
export function readReportOptions(options) {
  if (options === undefined || options === null) return [undefined, undefined]
  if (typeof options === 'string') return [options, undefined]
  if (typeof options !== 'object') {
    throw new TypeError(
      'ctx.report expects a message string or an options object — { message?, fix? }',
    )
  }

  const message = typeof options.message === 'string' ? options.message : undefined
  const fix = options.fix
  if (fix === undefined || fix === null) return [message, undefined]

  return [
    message,
    {
      node: fix.node,
      text: fix.text,
      // `option<bool>`, and the host decides what unspecified means: not safe. Sending `false`
      // here for an omitted `safe` would make this file the place that decided, which is
      // exactly what the world's doc comment asks an authoring language not to do.
      safe: typeof fix.safe === 'boolean' ? fix.safe : undefined,
    },
  ]
}

/**
 * Run a tracked read, turning the world's `read-error` back into a thrown refusal.
 *
 * A refusal throws; an absence returns `undefined`. The distinction is the point: "not there"
 * is an ordinary answer a rule should handle, and "you tried to leave the project" is a bug in
 * the rule.
 *
 * The three messages are `lanekeep_core::ReadError`'s own, word for word, so that a rule
 * author sees one refusal rather than one per engine. The variant carries only the path, so
 * the prose has to be composed on this side.
 *
 * @param {() => unknown} read
 * @param {string} path The path as the rule wrote it.
 */
function unwrapRead(read, path) {
  try {
    return read()
  } catch (refusal) {
    throw new Error(refusalMessage(refusal, path))
  }
}

/**
 * `lanekeep_core::ReadError`'s rendering, for the variant the host refused with.
 *
 * @param {unknown} refusal The payload of the world's `read-error`.
 * @param {string} path
 * @returns {string}
 */
function refusalMessage(refusal, path) {
  const tag = refusal?.payload?.tag ?? refusal?.tag
  switch (tag) {
    case 'escapes-root':
      return (
        `cannot read \`${path}\`\n  ` +
        'it resolves outside the project root, and rules may only read files within it'
      )
    case 'absolute':
      return (
        `cannot read \`${path}\`\n  ` +
        'reads are relative to the project root — an absolute path would make the rule ' +
        'depend on where the project happens to be checked out'
      )
    case 'not-text':
      return (
        `cannot read \`${path}\` as text: it is not valid UTF-8\n  ` +
        'use ctx.fileExists if the question is whether it is there'
      )
    default:
      // Not a `read-error` at all — a trap on the way out, or a shape this file does not know.
      // Handing back whatever came is better than inventing a refusal that did not happen.
      return refusal instanceof Error ? refusal.message : String(refusal)
  }
}
