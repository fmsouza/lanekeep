import { defineRule } from 'lanekeep'

/**
 * `.unwrap()` and `.expect()` outside tests.
 *
 * A library that panics on a malformed input has failed at its job: the caller wanted an
 * error it could handle and got a process abort instead. In a binary it is a crash with a
 * stack trace pointing at the unwrap rather than at the thing that was actually wrong.
 *
 * This is an architectural rule because the alternative is a convention nobody can enforce by
 * reading a diff — `?` and a typed error are more work at the call site, and the shortcut is
 * always available.
 *
 * In a test, panicking *is* the failure mechanism, so `#[test]` functions and files under a
 * `tests/` directory are not reported. That mirrors what a Rust project already does with
 * clippy's `unwrap_used`, and skipping it here would mean either a lint nobody can turn on or
 * a suppression on every assertion.
 *
 * @example
 * ```ts
 * export default defineConfig({
 *   rules: [{ rule: 'lanekeep/no-unwrap', options: { allow: ['src/main.rs'] } }],
 * })
 * ```
 */
export default function noUnwrap(options) {
  const allow = options?.allow ?? []

  return defineRule({
    id: 'lanekeep/no-unwrap',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'unwrap or expect outside a test',
      remediation:
        'propagate with `?` and a typed error, so the caller decides what a failure means',
      examples: {
        bad: 'let config = load().unwrap();',
        good: 'let config = load()?;',
      },
    },

    // No `fileContains` gate, deliberately.
    //
    // It would be the obvious optimization and it is not expressible: `fileContains` is an
    // *and* — every listed substring must be present — so `['unwrap', 'expect']` rejects any
    // file containing only one of them, which is nearly all of them. The failure is silent:
    // the rule loads, runs on nothing, and reads as a codebase with no unwraps in it.
    //
    // There is no single substring covering both, so the gate is omitted rather than written
    // wrong. The query is still the real gate; this only costs a parse on files that would
    // have been rejected.

    query: `
      (call_expression
        function: (field_expression
          field: (field_identifier) @method)) @call
    `,

    check(ctx, m) {
      const method = ctx.text(m.method)
      if (method !== 'unwrap' && method !== 'expect') return

      const path = ctx.filePath

      // An integration test directory, and the conventional unit-test module name. Panicking
      // is the failure mechanism there, which is the whole point of a test.
      if (path.includes('/tests/') || path.startsWith('tests/')) return
      if (inTestCode(ctx, m.call)) return

      for (const pattern of allow) {
        if (matches(pattern, path)) return
      }

      ctx.report(m.call, {
        message: `\`${method}()\` aborts the process where the caller wanted an error it could handle`,
      })
    },
  })
}

/**
 * Whether this node sits inside `#[test]` or `#[cfg(test)]`.
 *
 * Walked upwards rather than matched in the query, because the attribute is a *sibling* of
 * the item it applies to rather than a child — a query anchored on the attribute could not
 * also capture the call inside the function it decorates.
 */
function inTestCode(ctx: any, node: any): boolean {
  const chain = ctx.ancestors(node)

  for (let i = 0; i < chain.length; i++) {
    const ancestor = chain[i]
    const kind = ctx.kind(ancestor)
    if (kind !== 'function_item' && kind !== 'mod_item') continue

    // The next entry in the chain, rather than `ctx.parent(ancestor)`.
    //
    // Nodes cross the boundary as integer handles and the root's is `0`, so `if (!parent)`
    // discards it — every top-level item then looks parentless and no `#[test]` is ever
    // found. Reading the chain sidesteps the question entirely.
    const parent = chain[i + 1]
    if (parent === undefined) continue

    // Attributes are *preceding siblings*, so walk the parent's children and keep the run
    // of `attribute_item`s immediately before this item. Reading the item's own children
    // finds nothing — which is a rule that silently never exempts anything.
    let attached: string[] = []
    for (const sibling of ctx.namedChildren(parent)) {
      if (ctx.kind(sibling) === 'attribute_item') {
        attached.push(ctx.text(sibling))
        continue
      }
      if (ctx.line(sibling) === ctx.line(ancestor) && ctx.column(sibling) === ctx.column(ancestor)) {
        if (attached.some((a) => /\btest\b/.test(a))) return true
        break
      }
      // Any other item ends the run: those attributes belonged to it, not to us.
      attached = []
    }
  }
  return false
}

/** A `*` wildcard match, anchored at both ends. */
function matches(pattern: string, value: string): boolean {
  const escaped = pattern.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')
  return new RegExp(`^${escaped}$`).test(value)
}
