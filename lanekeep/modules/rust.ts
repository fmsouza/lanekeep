/**
 * Helpers shared by this repository's Rust rules.
 *
 * A module rather than a copy in each rule: `ruleset_hash` covers every module in the import
 * graph, so editing this file invalidates the cache for every rule that imports it. That is
 * the behavior §8.1 describes, and nothing else in this repository's own use exercises it.
 *
 * `inTestCode` also exists, verbatim, inside `crates/lanekeep-rules/rules/no-unwrap.ts`, and
 * that copy cannot be removed. Built-in rules resolve modules through the embedded
 * `BUILT_IN_MODULES` map under the reserved `lanekeep/` prefix, which is a different
 * resolution space from this directory — a built-in importing `../modules/rust` would not
 * resolve. The duplication is necessary; the two copies *diverging* would not be.
 */

/**
 * Whether this node sits inside `#[test]` or `#[cfg(test)]`.
 *
 * Walked upwards rather than matched in the query, because the attribute is a *sibling* of
 * the item it applies to rather than a child — a query anchored on the attribute could not
 * also capture the node inside the function it decorates.
 */
export function inTestCode(ctx: any, node: any): boolean {
  const chain = ctx.ancestors(node)

  for (let i = 0; i < chain.length; i++) {
    const ancestor = chain[i]
    const kind = ctx.kind(ancestor)
    if (kind !== 'function_item' && kind !== 'mod_item') continue

    // The next entry in the chain, rather than `ctx.parent(ancestor)`. Nodes cross the
    // boundary as integer handles and the root's is `0`, so `if (!parent)` discards it and
    // every top-level item looks parentless.
    const parent = chain[i + 1]
    if (parent === undefined) continue

    // Attributes are *preceding siblings*, so walk the parent's children and keep the run of
    // `attribute_item`s immediately before this item. Reading the item's own children finds
    // nothing, which is a rule that silently never exempts anything.
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

/**
 * Whether this node sits inside a larger path expression.
 *
 * A query alternating over `(use_declaration argument: (_))` and `(scoped_identifier path:
 * (_))` matches a single site more than once. `use std::process::Command;` matches the
 * declaration and the path nested in it. `std::process::Command::new()` matches once per
 * `::`, because a qualified path nests one `scoped_identifier` per segment — so a rule that
 * reports every match reports the same line three times.
 *
 * Reporting only the outermost match keeps one violation per site, which is what a reader
 * expects and what the tests assert.
 */
export function isNestedInPath(ctx: any, node: any): boolean {
  for (const ancestor of ctx.ancestors(node)) {
    const kind = ctx.kind(ancestor)
    if (kind === 'use_declaration' || kind === 'scoped_identifier') return true
  }
  return false
}
