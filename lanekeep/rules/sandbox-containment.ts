import { defineRule } from 'lanekeep'
import { underUse } from '../modules/rust'

/**
 * `rquickjs` named outside `lanekeep-js`.
 *
 * §5.1: there is no engine trait, deliberately, and what exists instead is containment. Every
 * line that knows QuickJS exists lives in `lanekeep-js`, behind `Sandbox` and the host
 * context, and no other crate names `rquickjs` at all. Swapping the engine then means
 * rewriting one crate against an interface its callers already use.
 *
 * That claim is true today and is held by nothing but review, which is what this rule fixes.
 * `allow` carries the one crate the containment is *for*.
 */
export default function sandboxContainment(options) {
  const allow = options?.allow ?? []

  return defineRule({
    id: 'local/sandbox-containment',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'the JavaScript engine named outside lanekeep-js',
      remediation: 'reach the sandbox through `Sandbox` and the host context, which is the interface a second engine would implement',
      examples: {
        bad: 'use rquickjs::Ctx;',
        good: 'use lanekeep_js::Sandbox;',
      },
    },

    gates: { fileContains: ['rquickjs'] },

    query: `
      [
        (use_declaration argument: (_) @path)
        (scoped_identifier path: (_) @path)
      ] @site
    `,

    check(ctx, m) {
      if (allow.some((prefix: string) => ctx.filePath.startsWith(prefix))) return

      if (ctx.kind(m.site) !== 'use_declaration') {
        // A multi-segment path nests one `scoped_identifier` inside another, one level
        // per `::` — `rquickjs::Ctx` is the `path` field of `rquickjs::Ctx::new`, and both
        // independently satisfy this same query. Left unchecked, one qualified reference
        // reports once per segment. Skip a site that is itself the `path` of an enclosing
        // match — a `use_declaration` via `underUse`, or another `scoped_identifier` here —
        // so only the outermost site in the chain reports.
        if (underUse(ctx, m.site)) return
        if (ctx.ancestors(m.site).some((a: any) => ctx.kind(a) === 'scoped_identifier')) return
      }

      const text = ctx.text(m.path)
      if (!/(^|::)rquickjs($|::)/.test(text)) return

      ctx.report(m.site, {
        message: `\`${text}\` names the JavaScript engine outside lanekeep-js, which is the containment §5.1 has instead of a trait`,
      })
    },
  })
}
