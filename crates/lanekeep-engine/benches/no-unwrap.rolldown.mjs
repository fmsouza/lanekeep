/**
 * What `jco componentize` may resolve while bundling `benches/no-unwrap-entry.ts`.
 *
 * Two specifiers, both of them lanekeep's own authoring package: `lanekeep` for the `defineRule`
 * the rule imports, and `lanekeep/runtime/entry` for the runtime the entry registers with.
 * Neither resolves by ordinary Node resolution from `crates/lanekeep-engine/benches/`, because
 * nothing there has a `node_modules` with lanekeep in it.
 *
 * # Deliberately *not* `crates/lanekeep-rules/typescript/rolldown.config.mjs`
 *
 * That file is the shipped build and it refuses any entry but its own — checked rather than
 * assumed, in its own `resolveId`. It also confines every rule source to
 * `crates/lanekeep-rules/`, which this rule is not in, and writes a source map beside the
 * shipped component, which a benchmark has no business rewriting.
 *
 * # And deliberately without the confinement half
 *
 * The shipped config enforces lanekeep's resolution rules — only `lanekeep`,
 * `lanekeep/<built-in>` and relative paths inside the rules root — because a rule compiled ahead
 * of time has no run time left to enforce them in. That is a property of the *product*. This
 * bundle has one hand-written entry and one rule source, both committed beside this file, and
 * restating the resolver here would be a second copy of it held honest by nothing. What the
 * benchmark needs from the build is that the guest is the same engine with the same globals
 * withheld, which is `_componentize`'s flags and the runtime import above — not the resolver.
 */

import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** `crates/lanekeep-engine/benches/`. */
const here = dirname(fileURLToPath(import.meta.url))

/** lanekeep's own authoring package, which is not rule code and does not live in a rules root. */
const packageRoot = resolve(here, '../../../packages/lanekeep')

/** The bare specifiers this bundle is allowed to name, and what each one is. */
const provided = {
  lanekeep: join(packageRoot, 'index.js'),
  'lanekeep/runtime/entry': join(packageRoot, 'runtime/entry.js'),
}

export default {
  plugins: [
    {
      name: 'lanekeep-bench',
      resolveId(specifier) {
        return provided[specifier] ?? null
      },
    },
  ],
}
