import { defineRule } from 'lanekeep'

/**
 * A test that asserts nothing passes forever and covers nothing.
 *
 * Agents pad coverage on request — a body that calls the subject and checks nothing is the
 * cheapest way to make a coverage number move — so this fires often and early in
 * agent-written code. One rule, four language families: what is per-language is how a test
 * is recognized and what counts as asserting; the judgment is the same everywhere.
 *
 * | language | a test is | asserts by default |
 * | --- | --- | --- |
 * | typescript/tsx | an `it(...)`/`test(...)` call, a modifier form like `test.only(...)`, or a table form like `it.each(table)(...)`, with a block-bodied callback | `expect*`, `assert*` |
 * | python | a `def test*` function, methods included | the `assert` statement, `self.assert*`, `self.fail`, `pytest.raises` |
 * | go | `func Test*` taking `*testing.T` | `t.Error*`, `t.Fatal*`, `t.Fail*`, `assert.*`, `require.*` |
 * | rust | a `fn` under `#[test]` or a `::test` attribute path | `assert*!`, `debug_assert*!`, `panic!` |
 *
 * Two exemptions are correctness rather than convenience: a go test that calls `t.Skip*`
 * and a rust test under `#[should_panic]` legitimately assert nothing.
 *
 * Vocabulary entries are matched as *prefixes* of the normalized callee (whitespace
 * stripped, `?.` folded to `.`), so `t.Error` covers `t.Errorf` and `self.assert` covers
 * every `self.assert*` method. The rule does not chase helpers: an assertion inside a
 * function the test calls is invisible here, which is the same limit `expect-expect` has —
 * name such helpers in `allowHelpers` and they count as asserting in every language.
 *
 * A typescript test callee is `it`, `test`, a name listed in `testCallees` — a fixture-extended
 * `test` exported under another name — or an alias of a test framework's `it`/`test`, followed
 * through its import binding (`import { it as base } from 'vitest'`); every one of them takes
 * the modifier and table forms.
 *
 * Known limits, deliberate for v1: go's receiver is matched by its conventional name (a
 * `func TestX(tt *testing.T)` calling `tt.Error` needs `assertions: { go: ['tt.'] }`), and
 * table-driven tests whose assertion lives in a loop body are covered only because the
 * loop is still inside the test's block.
 *
 * @example
 * ```ts
 * import noAssertionlessTest from 'lanekeep/no-assertionless-test'
 *
 * export default defineConfig({
 *   rules: [
 *     noAssertionlessTest({
 *       tests: ['tests/**', 'src/**'],
 *       assertions: { go: ['suite.'] },
 *       allowHelpers: ['expectValidResponse'],
 *       testCallees: ['authTest'],
 *     }),
 *   ],
 * })
 * ```
 */
export default function noAssertionlessTest(options) {
  // The ignored-options trap: options reach a rule only by being closed over.
  const tests = options?.tests
  const extra = options?.assertions ?? {}
  const helpers = options?.allowHelpers ?? []
  const callees = [...TEST_CALLEES, ...testCalleesOf(options)]

  const vocabulary = (family) => [
    ...DEFAULT_ASSERTIONS[family],
    ...(extra[family] ?? []),
    ...helpers,
  ]

  return defineRule({
    id: 'lanekeep/no-assertionless-test',
    language: ['typescript', 'tsx', 'python', 'go', 'rust'],
    severity: 'error',

    card: {
      message: 'assertionless test',
      remediation:
        'assert an observable outcome — a test no outcome can fail protects nothing; if it legitimately cannot assert, skip it or mark it (t.Skip, #[should_panic])',
      examples: {
        bad: "it('adds', () => {\n  add(1, 2)\n})",
        good: "it('adds', () => {\n  expect(add(1, 2)).toBe(3)\n})",
      },
    },

    // The one gate a multi-token judgment can have: where the tests live. Only set when
    // the config says — rust unit tests conventionally live inline in `src/*.rs`, so a
    // default glob would silently exclude them, and a gate that is wrong is worse than
    // none (the `fileContains` entry in AGENTS.md, transposed to paths).
    gates: tests ? { pathMatches: tests } : {},

    // One query per grammar: each matches a *candidate* test definition and captures the
    // body the handler scans. The candidate is narrowed in the handler — by callee name
    // for typescript, by name for python, by name and parameter for go, by attribute for
    // rust — because the narrowing needs text, which a query cannot compare.
    query: {
      typescript: TS_QUERY,
      tsx: TS_QUERY,
      python: `
        (function_definition
          name: (identifier) @name
          body: (block) @body) @def
      `,
      go: `
        (function_declaration
          name: (identifier) @name
          parameters: (parameter_list) @params
          body: (block) @body) @def
      `,
      rust: `
        (function_item
          name: (identifier) @name
          body: (block) @body) @def
      `,
    },

    check(ctx, m) {
      const family = familyOf(ctx.filePath)

      if (family === 'typescript') {
        const base = testShapeBase(normalize(ctx.text(m.fn)), m.table !== undefined)
        if (base === undefined) return
        if (!callees.includes(base) && !isFrameworkAlias(ctx, m.fn, base)) return
        if (asserts(ctx, m.body, CALLS.typescript, vocabulary('typescript'))) return
        ctx.report(m.def, 'test asserts nothing')
        return
      }

      const name = ctx.text(m.name)

      if (family === 'python') {
        if (!name.startsWith('test')) return
        // `assert` is a statement, not a call — the node query is the half of the
        // vocabulary a name list cannot carry.
        if (ctx.querySubtree(m.body, '(assert_statement) @a').length > 0) return
        if (asserts(ctx, m.body, CALLS.python, vocabulary('python'))) return
        ctx.report(m.def, `test '${name}' asserts nothing`)
        return
      }

      if (family === 'go') {
        if (!name.startsWith('Test')) return
        // The parameter is what makes go's convention a convention: `TestHelper(data
        // string)` is a name collision, not a test.
        if (!ctx.text(m.params).includes('testing.T')) return
        if (asserts(ctx, m.body, CALLS.go, EXEMPT_GO)) return
        if (asserts(ctx, m.body, CALLS.go, vocabulary('go'))) return
        ctx.report(m.def, `test '${name}' asserts nothing`)
        return
      }

      if (family === 'rust') {
        const attributes = attributesOf(ctx, m.def)
        if (!attributes.some(isTestAttribute)) return
        if (attributes.some((a) => a.includes('should_panic'))) return
        if (asserts(ctx, m.body, CALLS.rustMacros, vocabulary('rust'))) return
        if (asserts(ctx, m.body, CALLS.rustCalls, vocabulary('rust'))) return
        ctx.report(m.def, `test '${name}' asserts nothing`)
      }
    },
  })
}

/**
 * The typescript grammar's test shape; tsx shares the vocabulary, so both entries use it.
 *
 * `@fn` is the whole callee — `test`, or `test.only` — not only its object: capturing the
 * object alone made every `test.<member>(...)` a test, Playwright's hooks and steps included
 * (#287). Which callees declare a test is `isTestCallee`'s judgment, made on the text.
 *
 * The third shape is a table-driven test, `it.each(table)(name, fn)` or its tagged-template
 * form with a template-literal table (#288): the callback sits in the *outer* call, whose
 * callee is itself a call. There `@fn` is that inner call's callee — `it.only.each`, never
 * the table — and `@table` is captured only for this shape, which is how the handler tells
 * the forms apart. Matched on its own by the second shape, the inner call is never a test:
 * its callee ends in `each`, which is not a modifier.
 */
const TS_QUERY = `
  (call_expression
    function: [
      (identifier) @fn
      (member_expression object: (identifier) property: (property_identifier)) @fn
      (call_expression
        function: (member_expression property: (property_identifier)) @fn) @table
    ]
    arguments: (arguments [
      (arrow_function body: (statement_block) @body)
      (function_expression body: (statement_block) @body)
    ])) @def
`

/** The names a typescript test is declared by, before `testCallees` adds a project's own. */
const TEST_CALLEES = ['it', 'test']

/**
 * The members of a test callee that still declare a test: jest and vitest's `only`, `skip`,
 * `concurrent`, jest's `failing`, vitest's `fails`, Playwright's `fail` and `fixme`.
 *
 * An allow-list, so an unknown member is not a test. Every other `test.<member>` —
 * `beforeEach`, `afterAll`, `describe`, `step`, `use`, `extend` — is a hook, a group or
 * configuration, none of which is expected to assert. A miss here is a test the rule does
 * not see; a deny-list's miss would be correct code reported.
 */
const TEST_MODIFIERS = ['only', 'skip', 'concurrent', 'fails', 'failing', 'fail', 'fixme']

/**
 * The base a normalized callee would declare a test with, when its shape is a test's: the
 * first segment, followed by modifiers only. `undefined` when the shape is not.
 *
 * Whether the base is a test callee is the caller's question — by name, against `it`, `test`
 * and `testCallees` — so every name gets every modifier and the table form at no cost.
 *
 * With `table`, the callee is the one a table call was made on, so it must end in `.each`,
 * and the modifiers are the segments before it — jest documents them chained, as in
 * `test.concurrent.only.each`. `each` is not a modifier: `it.each(name, fn)` called directly
 * declares nothing. How many modifiers the plain form can carry is the query's to say; it
 * admits one.
 */
function testShapeBase(callee, table) {
  const segments = callee.split('.')
  if (table && segments.pop() !== 'each') return undefined
  const [base, ...modifiers] = segments
  if (!modifiers.every((modifier) => TEST_MODIFIERS.includes(modifier))) return undefined
  return base
}

/**
 * The modules whose `it` and `test` exports declare a test under whatever local name they are
 * imported as — `import { it as base } from 'vitest'`.
 *
 * A fixed list because the host answers "is this the export `name` of module `m`" only for an
 * exact `m`; nothing asks which export a binding is without naming its module. A fixture
 * module's `test` (`import { test as pw } from './fixtures'`) is therefore not followed — name
 * it in `testCallees`.
 */
const TEST_MODULES = [
  'vitest',
  '@jest/globals',
  '@playwright/test',
  'bun:test',
  'node:test',
  'mocha',
]

/**
 * Whether the callee's base binds to a test framework's `it` or `test`, under another name.
 *
 * Asked last, since it is the only part of the judgment that crosses into the host, and only
 * for a test-shaped callee whose base is no known name — every `useEffect(() => {...})` in a
 * React file is one. So the modules are first narrowed in the sandbox, with no crossing, by
 * `quotedTestModules`. Most files quote none, and stop there. Only then is the base's binding
 * resolved, once per module the text names, and the export asked about only for the module it
 * is from. Binding-exact: a local `base` shadowing the import resolves to the local and is not
 * a test.
 */
function isFrameworkAlias(ctx, callee, base) {
  if (!isIdentifier(base)) return false
  const quoted = quotedTestModules(ctx.fileText)
  if (quoted.length === 0) return false
  const node = baseIdentifier(ctx, callee)
  if (node === undefined) return false
  return quoted.some(
    (module) =>
      ctx.resolvesToImport(node, module) &&
      TEST_CALLEES.some((name) => ctx.resolvesToImport(node, module, name)),
  )
}

/**
 * The framework modules a file's text quotes — the only ones an import in it can bind from,
 * since a static import spells its specifier as a string literal.
 *
 * Remembered for the last text asked about, because it is asked once per candidate and is the
 * same answer for every candidate in a file. On a hook-heavy TSX corpus both alternatives made
 * the whole run several times slower — scanning the text once per candidate, and asking the host
 * instead, one binding resolution per module for every hook call; the pull request that added
 * this has the measurements.
 *
 * The memo is not state in the sense the determinism invariant forbids: its value is a pure
 * function of its key, and the key is the whole text, compared by content. Two files with the
 * same text share an answer because they have the same answer; no order of files, workers or
 * runs can change what it returns.
 */
function quotedTestModules(text) {
  if (text !== lastQuoted.text) {
    lastQuoted = {
      text,
      modules: TEST_MODULES.filter(
        (module) => text.includes(`'${module}'`) || text.includes(`"${module}"`),
      ),
    }
  }
  return lastQuoted.modules
}

let lastQuoted = { text: undefined, modules: [] }

/**
 * The identifier a callee starts with — `pw` in `pw`, `pw.only` and `pw.only.each` — reached
 * through each `member_expression`'s object, its first named child that is not a comment.
 * Compared with `undefined`, never truthiness: a handle is an integer and may be `0`.
 */
function baseIdentifier(ctx, callee) {
  let node = callee
  for (;;) {
    const kind = ctx.kind(node)
    if (kind === 'identifier') return node
    if (kind !== 'member_expression') return undefined
    node = ctx.namedChildren(node).find((child) => ctx.kind(child) !== 'comment')
    if (node === undefined) return undefined
  }
}

/**
 * The `testCallees` option, refused when it could not mean anything.
 *
 * Each entry is a plain identifier because it is compared with a callee's *base*: a dotted
 * entry (`test.describe`) could never equal one, and a string in place of the array would turn
 * the membership test into a substring test. Either would be an option silently ignored —
 * lowering what the rule sees with no sign — so both refuse to load instead.
 */
function testCalleesOf(options) {
  const names = options?.testCallees
  if (names === undefined) return []
  if (!Array.isArray(names) || !names.every(isIdentifier)) {
    throw new Error(
      'no-assertionless-test: `testCallees` must be an array of plain identifiers, like ' +
        `['pw'] — got ${JSON.stringify(names)}`,
    )
  }
  return names
}

/** A string that could be a callee's base; `RegExp#test` alone would coerce `true` to one. */
function isIdentifier(name) {
  return typeof name === 'string' && /^[A-Za-z_$][\w$]*$/.test(name)
}

/** What counts as asserting when nothing is configured, per language family. */
const DEFAULT_ASSERTIONS = {
  typescript: ['expect', 'assert'],
  python: ['pytest.raises', 'self.assert', 'self.fail'],
  go: ['t.Error', 't.Fatal', 't.Fail', 'assert.', 'require.'],
  rust: ['assert', 'debug_assert', 'panic'],
}

/** A skipped go test asserts nothing on purpose. */
const EXEMPT_GO = ['t.Skip']

/** The callee-shaped query for each family's assertion scan. */
const CALLS = {
  typescript: '(call_expression function: [(identifier) (member_expression)] @callee) @c',
  python: '(call function: [(identifier) (attribute)] @callee) @c',
  go: '(call_expression function: [(identifier) (selector_expression)] @callee) @c',
  rustMacros: '(macro_invocation macro: [(identifier) (scoped_identifier)] @callee) @c',
  rustCalls:
    '(call_expression function: [(identifier) (scoped_identifier) (field_expression)] @callee) @c',
}

/** Whether any call in `body` has a callee one of `names` prefixes. */
function asserts(ctx, body, query, names) {
  for (const match of ctx.querySubtree(body, query)) {
    const callee = normalize(ctx.text(match.callee))
    if (names.some((name) => callee.startsWith(name))) return true
  }
  return false
}

/** Whitespace stripped, `?.` folded to `.` — the same normalization the callee rules use. */
function normalize(text) {
  return text.replace(/\s+/g, '').replace(/\?\./g, '.')
}

/** Which language family a subject file belongs to, from its extension. */
function familyOf(path) {
  const extension = path.slice(path.lastIndexOf('.') + 1)
  if (extension === 'py') return 'python'
  if (extension === 'go') return 'go'
  if (extension === 'rs') return 'rust'
  return 'typescript'
}

/**
 * The attribute texts sitting directly above a rust item, normalized.
 *
 * Attributes are *sibling* `attribute_item` nodes preceding the `function_item`, so this
 * walks backwards from the item through its parent's children, stepping over comments.
 * The handle comparison is `=== undefined` on purpose — a node handle is an integer and
 * the root's is `0`, so a truthiness check would discard a real node.
 */
function attributesOf(ctx, def) {
  const parent = ctx.parent(def)
  if (parent === undefined) return []

  const siblings = ctx.children(parent)
  const at = siblings.indexOf(def)
  const found = []
  for (let i = at - 1; i >= 0; i -= 1) {
    const kind = ctx.kind(siblings[i])
    if (kind === 'line_comment' || kind === 'block_comment') continue
    if (kind !== 'attribute_item') break
    found.push(normalize(ctx.text(siblings[i])))
  }
  return found
}

/** `#[test]`, or a path attribute whose last segment is `test` — `#[tokio::test]`. */
function isTestAttribute(text) {
  return text === '#[test]' || text.endsWith('::test]')
}
