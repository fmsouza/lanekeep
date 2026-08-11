/**
 * The entry module of the TypeScript built-ins component.
 *
 * `just typescript-builtins` bundles this with `jco componentize` into
 * `../components/typescript-builtins.wasm`, which is committed and reaches the binary through
 * `include_bytes!`. Nothing in any gate builds it, exactly as nothing in any gate builds the
 * components under `rust-rules/`.
 *
 * # One component for every TypeScript rule, not one each
 *
 * Measured 2026-08-10 with jco 1.27.0: a component hosting one rule is 12,941,489 bytes and one
 * hosting four is 12,951,191 — **9,702 bytes for three more rules**. The engine is the cost and
 * the rules are rounding error, so a component per rule would multiply a 12.4 MiB
 * StarlingMonkey build by the number of rules that ship. `world rule` takes a rule index on
 * every export for this reason, and `lanekeep_rules::component` maps a name to
 * `(component, index)`.
 *
 * # The runtime import is first, and that ordering is load-bearing
 *
 * Importing `lanekeep/runtime/entry` runs `withhold()` at its top level, which is what takes
 * `Date`, `Math.random`, `fetch` and the rest away from rule code. ES modules evaluate
 * depth-first in the order their imports are written, and rolldown preserves that order when it
 * flattens them into one module — so a rule imported *before* the runtime would have its module
 * scope evaluated while the clock was still reachable, and could capture it. Nothing in a rule's
 * `check` or `reduce` is affected either way, because those run long after both; module scope is
 * the whole of the window.
 *
 * `crates/lanekeep-wasm/tests/fixtures/js-globals/` is where that is *demonstrated* rather than
 * described — `probe.js` reads the clock at module scope and reports what it found, from a
 * module the entry imports after the runtime.
 *
 * # The order of the four
 *
 * Alphabetical, and it is an interface rather than a preference: the index a rule sits at here
 * is the index `lanekeep_rules`' name-to-rule table dispatches on, and the component's own
 * `rules()` is asserted against that table. Inserting a rule in the middle renumbers every rule
 * after it, so a new one goes wherever the alphabet puts it and the table is re-recorded.
 */

// First. See above — this is not import hygiene, it is the sandbox.
import { register } from 'lanekeep/runtime/entry'

import noCircularImports from '../rules/no-circular-imports.ts'
import noDefaultExport from '../rules/no-default-export.ts'
import noRestrictedImports from '../rules/no-restricted-imports.ts'
import noUnusedExports from '../rules/no-unused-exports.ts'

// Rule objects and factories both, which `register` and `configure` between them sort out:
// `no-default-export` is a rule, the other three are functions over their options.
register([noCircularImports, noDefaultExport, noRestrictedImports, noUnusedExports])

// The seven exports `world rule` declares. Named rather than `export * from`, which would also
// export `register` — a build-time function that has no place in a component's public shape.
export {
  rules,
  metadata,
  configure,
  hasCheck,
  hasReduce,
  check,
  reduce,
} from 'lanekeep/runtime/entry'
