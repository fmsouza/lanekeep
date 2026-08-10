/**
 * Types reached through the `typesVersions` mapping in `package.json`.
 *
 * That mapping points *every* specifier here, the bare `lanekeep` included — TypeScript's
 * `"*"` pattern does not exclude the package root, and a narrower pattern would have to
 * predict what future built-ins are called. So this file is a superset: it re-exports
 * everything `index.d.ts` has, and adds the default export a built-in subpath needs.
 *
 * A `declare module 'lanekeep/*'` block inside `index.d.ts` would have been the obvious way
 * to do this and does nothing at all: a `declare module` inside a file that has its own
 * imports or exports is module augmentation, not an ambient declaration, so TypeScript
 * ignores it and the import stays unresolved. That failed silently until a compile test
 * caught it.
 *
 * The default covers the two shapes an *importable* built-in can take, because which one it is
 * cannot be known from the specifier: a rule taking options is a factory —
 * `noRestrictedImports({ ... })` — and one taking none is the rule itself.
 *
 * **There is a third shape, and this file types it wrongly.** A built-in may ship as a
 * WebAssembly component, which has no module to import at all: `lanekeep/no-unwrap` and
 * `lanekeep/no-glob-import` are components today. The `typesVersions` mapping points every
 * specifier here, so importing one type-checks and then fails at run time with a resolution
 * error naming the reason. Name it in `lanekeep.json` instead — `"rules": ["lanekeep/no-unwrap"]`
 * — which is the spelling `lanekeep init` scaffolds and which works whichever form the rule
 * takes.
 *
 * Typing that honestly needs the mapping to distinguish specifiers, which means either
 * enumerating built-ins in `package.json` — a list that goes stale on the next migration — or
 * generating it. Left as follow-up; this comment exists so the gap is not mistaken for a
 * complete enumeration. There is no `tsc` gate over this package, so nothing here is red.
 */
export * from './index'

import type { Rule } from './index'

declare const rule: Rule & ((options?: Record<string, unknown>) => Rule)
export default rule
