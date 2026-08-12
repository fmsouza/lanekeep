import { defineRule } from 'lanekeep'

/**
 * The tree API called from a `reduce` handler.
 *
 * Invariant 1 in §2: the reduce phase never touches parse trees. Facts are small and
 * serializable, which is what keeps cross-file rules parallel and cacheable; handing a tree to
 * `reduce` would make the whole corpus resident and kill incrementality.
 *
 * The host enforces the split by *shape* — `facts` and `files` do not exist in the per-file
 * context, and `emitFact` does not exist in the reduce one. That is the strong version and it
 * holds. What it does not cover is a rule author reaching for `ctx.text` in `reduce` and
 * getting a run-time throw instead of a diagnostic at authoring time, which is what this rule
 * turns it into.
 */
const TREE_API = [
  'text',
  'kind',
  'parent',
  'children',
  'namedChildren',
  'ancestors',
  'querySubtree',
  'closestAncestor',
  'line',
  'column',
  'isNamed',
  'root',
  'fileText',
  'filePath',
  'emitFact',
  // These four take a `Node` too, and are absent from `ReduceContext` the same way the
  // navigation methods above are. They read like queries rather than like tree walking,
  // which is exactly why they are easy to forget here.
  'resolvesToImport',
  'isImportedFrom',
  'bindingKind',
  'isShadowed',
]

export default defineRule({
  id: 'local/reduce-touches-no-tree',
  language: ['typescript', 'tsx'],
  severity: 'error',

  card: {
    message: 'tree API called from reduce',
    remediation: 'capture what you need during `check` and emit it as a fact — reduce receives facts and the file list, and no trees',
    examples: {
      bad: 'reduce(ctx) { const name = ctx.text(node) }',
      good: "check(ctx, m) { ctx.emitFact({ kind: 'e', name: ctx.text(m.d) }) }",
    },
  },

  gates: { fileContains: ['reduce'] },

  query: '(method_definition name: (property_identifier) @name body: (statement_block) @body) @method',

  check(ctx, m) {
    if (ctx.text(m.name) !== 'reduce') return

    for (const hit of ctx.querySubtree(
      m.body,
      '(member_expression property: (property_identifier) @prop) @expr',
    )) {
      const prop = ctx.text(hit.prop)
      if (!TREE_API.includes(prop)) continue
      if (!ctx.text(hit.expr).startsWith('ctx.')) continue

      ctx.report(hit.expr, {
        message: `\`ctx.${prop}\` needs a parse tree, and reduce has none — capture it during check and emit a fact`,
      })
    }
  },
})
