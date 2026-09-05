import { defineRule } from 'lanekeep'

/**
 * A secret value reaching a string sink — logged, concatenated, or otherwise turned into
 * text — without passing through a sanitizer first.
 *
 * The first built-in driven by `checkFlow` rather than `check`: it declares
 * `requires: ['dataflow']` and a `flow` — `sources`/`sinks`/`sanitizers` queries the engine
 * runs natively — instead of a `query`/`check` pair. The engine resolves every source-to-sink
 * path itself (def-use through assignments and local aliases, cut at a sanitizer) and calls
 * `checkFlow` once per canonical path, reporting at the sink.
 *
 * This ships mainly as the acceptance vehicle for the taint-analysis surface — see
 * `docs/superpowers/specs/2026-09-05-taint-analysis-flow-checkflow-design.md` — and as the
 * worked example for a project writing its own dataflow rule. `getSecret`, `log` and `redact`
 * are illustrative names, not a real project's API; a real config names its own.
 *
 * **v1 is intra-procedural, path-insensitive, and field-insensitive** — see
 * `crates/lanekeep-lang-js/src/flow.rs` for the analysis this rule rides on. Concretely: taint
 * is not followed through a call's own arguments (`identity(secret)` loses it), a branch guard
 * does not suppress a report reachable from another branch, and tainting one field of an
 * object taints reads of every field of it. All three are the documented trade of a
 * may-analysis that leans toward false positives rather than false negatives; `sanitizers` is
 * the project-facing lever for narrowing either direction — see `docs/built-in-rules.md`.
 *
 * @example
 * ```ts
 * log(getSecret())          // reported at `getSecret()`
 * log(redact(getSecret()))  // sanitized first — silent
 * ```
 */
export default defineRule({
  id: 'lanekeep/no-secret-in-string',
  severity: 'error',
  requires: ['dataflow'],

  card: {
    message: 'A secret value reaches a string.',
    remediation: 'Redact it before it becomes a string.',
    examples: {
      bad: 'log(getSecret())',
      good: 'log(redact(getSecret()))',
    },
  },

  flow: {
    sources: ['(call_expression function: (identifier) @source (#eq? @source "getSecret"))'],
    sinks: [
      '(call_expression function: (identifier) @fn (#eq? @fn "log") arguments: (arguments (_) @sink))',
    ],
    sanitizers: [
      '(call_expression function: (identifier) @sanitizer (#eq? @sanitizer "redact"))',
    ],
  },

  checkFlow(ctx, path) {
    ctx.report(path.sink, 'A secret value reaches a string.')
  },
})
