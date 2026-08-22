import { defineRule } from 'lanekeep'

/**
 * A rule that does not name the languages it targets.
 *
 * `language` defaults to `['typescript', 'tsx']`, and a rule does not run on a file whose
 * language it does not name. So a rule written for Rust and shipped without the field runs on
 * nothing at all — no error, no warning, a clean report.
 *
 * The related failure is already in AGENTS.md and cost real work: before the file chose the
 * grammar, a rule declaring `language: 'typescript'` parsed `.tsx` files with the TypeScript
 * grammar, every JSX element became an `ERROR` node, and queries matched nothing inside them.
 * On a React Native codebase that hid most of the code and produced 2218 false positives in
 * one rule. Writing the field out is cheap; the failure it prevents is silent.
 */
export default defineRule({
  id: 'local/rule-declares-language',
  language: ['typescript', 'tsx'],
  severity: 'error',

  card: {
    message: 'rule does not declare its language',
    remediation: "name the languages explicitly — the default is ['typescript','tsx'], so a rule for anything else runs on no files and reports nothing",
    examples: {
      bad: "defineRule({ id: 'local/x', query: '...' })",
      good: "defineRule({ id: 'local/x', language: 'rust', query: '...' })",
    },
  },

  gates: { fileContains: ['defineRule'] },

  query: '(call_expression function: (identifier) @fn arguments: (arguments (object) @body)) @call',

  check(ctx, m) {
    if (ctx.text(m.fn) !== 'defineRule') return

    for (const pair of ctx.namedChildren(m.body)) {
      if (ctx.kind(pair) !== 'pair') continue
      const key = ctx.namedChildren(pair)[0]
      if (ctx.text(key) === 'language') return
    }

    ctx.report(m.call, {
      message: 'no `language`, so this rule runs on TypeScript and TSX only — which for any other target means it runs on nothing',
    })
  },
})
