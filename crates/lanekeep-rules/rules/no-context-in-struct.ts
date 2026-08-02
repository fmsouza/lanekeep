import { defineRule } from 'lanekeep'

/**
 * Storing a `context.Context` in a struct.
 *
 * The context package says it plainly: do not store Contexts inside a struct type; pass one
 * explicitly, as the first parameter. A stored context outlives the call it was scoped to,
 * so cancellation and deadlines stop meaning what the caller intended — a long-lived client
 * holds the context of whichever request happened to construct it, and cancelling that
 * request cancels work belonging to every other.
 *
 * It is an architectural rule rather than a style one: the damage shows up as unrelated
 * requests failing together, which is nearly impossible to attribute back to the field.
 *
 * The resolver is what keeps this from being a text match: `ctx.bindingKind` says whether
 * the qualifier is an import at all, so a package-level name that happens to read `context`
 * does not fire.
 *
 * What it deliberately does *not* distinguish is which module the import points at. The host
 * API answers what kind of binding a name has, not where it came from, so a package aliased
 * to `context` and exposing a `Context` is reported the same as the standard library's.
 * Widening the host API to expose the module would bump its version, which is a cache key
 * input — a bigger change than this rule justifies. The false positive it leaves is narrow
 * and, where it happens, is usually the same mistake wearing a different import path.
 *
 * @example
 * ```ts
 * import noContextInStruct from 'lanekeep/no-context-in-struct'
 *
 * export default defineConfig({ rules: [noContextInStruct] })
 * ```
 */
export default defineRule({
  id: 'lanekeep/no-context-in-struct',
  language: 'go',
  severity: 'error',

  card: {
    message: 'a context.Context is stored in a struct',
    remediation:
      'pass the context as the first parameter of each method instead, so its cancellation scope matches the call',
    examples: {
      bad: 'type Client struct {\n\tctx context.Context\n}',
      good: 'type Client struct{}\n\nfunc (c *Client) Do(ctx context.Context) error { return nil }',
    },
  },

  gates: { fileContains: ['context'] },

  // Two patterns rather than one: `ctx context.Context` and `ctx *context.Context` differ by
  // a `pointer_type` in between, and a query that matched only the bare form would pass the
  // pointer one silently. Both capture the same names, so `check` does not care which fired.
  query: `
    (field_declaration
      type: (qualified_type
        package: (package_identifier) @pkg
        name: (type_identifier) @name)) @field
    (field_declaration
      type: (pointer_type
        (qualified_type
          package: (package_identifier) @pkg
          name: (type_identifier) @name))) @field
  `,

  check(ctx, m) {
    if (ctx.text(m.name) !== 'Context') return
    if (ctx.text(m.pkg) !== 'context') return

    // A qualifier that is not an import is a local name that happens to read `context`, and
    // a type of the same shape from somewhere else entirely.
    if (ctx.bindingKind(m.pkg) !== 'import') return

    ctx.report(m.field, {
      message:
        'a context.Context stored in a struct outlives the call it was scoped to, so cancelling one request can cancel unrelated work',
    })
  },
})
