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
 * The default covers both shapes a built-in can take, because which one it is cannot be known
 * from the specifier: a rule taking options is a factory — `noRestrictedImports({ ... })` —
 * and one taking none is the rule itself.
 */
export * from './index'

import type { Rule } from './index'

declare const rule: Rule & ((options?: Record<string, unknown>) => Rule)
export default rule
