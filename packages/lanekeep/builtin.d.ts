/**
 * Types for the importable built-in subpaths — the module built-ins, reached through the
 * per-name `exports`/`typesVersions` entries `crates/lanekeep-package-gen` generates from
 * `COMPONENT_RULES`. A component built-in has no entry there, so importing one is a compile
 * error rather than a default export that lies.
 *
 * The default covers the two shapes an importable built-in can take, because which one it is
 * cannot be known from the specifier: a rule taking options is a factory —
 * `noRestrictedImports({ ... })` — and one taking none is the rule itself.
 */
export * from './index'

import type { Rule } from './index'

declare const rule: Rule & ((options?: Record<string, unknown>) => Rule)
export default rule
