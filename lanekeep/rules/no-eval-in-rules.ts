import { defineRule } from 'lanekeep'

/**
 * `eval` or the `Function` constructor in a rule module.
 *
 * §6.6: both are present in the sandbox and cannot be removed, because the engine's own script
 * evaluation depends on that intrinsic. This grants a rule no capability it lacks — a rule is
 * already arbitrary code — but it does mean reviewing a third-party rule cannot rely on
 * reading its source alone.
 *
 * Every built-in rule is reviewed by a maintainer (§13), and a rule that builds its behavior
 * at run time defeats that review. This is a rule about reviewability, not about confinement.
 */
export default defineRule({
  id: 'local/no-eval-in-rules',
  language: ['typescript', 'tsx'],
  severity: 'error',

  card: {
    message: 'eval or the Function constructor in a rule',
    remediation: 'write the logic out — a rule whose behavior is built at run time cannot be reviewed by reading it',
    examples: {
      bad: 'const check = new Function(source)',
      good: 'function check(ctx, m) { /* ... */ }',
    },
  },

  query: `
    [
      (call_expression function: (identifier) @name)
      (new_expression constructor: (identifier) @name)
    ] @site
  `,

  check(ctx, m) {
    const name = ctx.text(m.name)
    if (name !== 'eval' && name !== 'Function') return

    ctx.report(m.site, {
      message: `\`${name}\` builds behavior at run time, so reading this rule no longer tells a reviewer what it does`,
    })
  },
})
