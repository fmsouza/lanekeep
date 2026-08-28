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
 * | typescript/tsx | an `it(...)`/`test(...)` call with a block-bodied callback | `expect*`, `assert*` |
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
        const callee = normalize(ctx.text(m.fn))
        if (callee !== 'it' && callee !== 'test') return
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

/** The typescript grammar's test shape; tsx shares the vocabulary, so both entries use it. */
const TS_QUERY = `
  (call_expression
    function: [(identifier) @fn (member_expression object: (identifier) @fn)]
    arguments: (arguments [
      (arrow_function body: (statement_block) @body)
      (function_expression body: (statement_block) @body)
    ])) @def
`

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
