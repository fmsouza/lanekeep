import { defineRule } from 'lanekeep'

/**
 * `gates.fileContains` listing more than one substring.
 *
 * The gate is an *and*: every listed substring has to be present. So a rule matching either of
 * two tokens cannot express its gate as `['unwrap', 'expect']` — that rejects any file
 * containing only one of them, which is nearly all of them.
 *
 * Nothing fails. The rule loads, the query never runs, and the output reads exactly like a
 * codebase with none of the thing in it. `crates/lanekeep-rules/rules/no-unwrap.ts` carries a
 * comment explaining why it omits the gate it would obviously want, and this rule is what
 * makes that reasoning survive the next author.
 */
export default defineRule({
  id: 'local/gates-are-and',
  language: ['typescript', 'tsx'],
  severity: 'error',

  card: {
    message: 'content gate listing more than one substring',
    remediation: 'every listed substring must be present — use the one that covers the rule, or omit the gate and let the query be the gate',
    examples: {
      bad: "gates: { fileContains: ['unwrap', 'expect'] }",
      good: "gates: { fileContains: ['makeStyles'] }",
    },
  },

  // `Contains`, not `fileContains`: `fileNotContains` does not contain `fileContains` as a
  // substring — the `Not` breaks the run of characters — so that would gate out every file
  // that uses only the negative form, which is exactly the silent-rejection failure this rule
  // exists to catch. `Contains` is the substring both keys actually share. Broader than it
  // needs to be is fine; gates only cost an extra parse when they over-admit. Under-admitting
  // is the one thing the gate on this rule must never do.
  gates: { fileContains: ['Contains'] },

  query: `
    (pair
      key: (property_identifier) @key
      value: (array) @value) @pair
  `,

  check(ctx, m) {
    const key = ctx.text(m.key)
    if (key !== 'fileContains' && key !== 'fileNotContains') return

    const entries = ctx.namedChildren(m.value)
    if (entries.length < 2) return

    ctx.report(m.pair, {
      message: `\`${key}\` lists ${entries.length} substrings and requires all of them, so a file with only one is rejected — silently`,
    })
  },
})
