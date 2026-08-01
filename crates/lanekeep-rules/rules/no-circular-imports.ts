import { defineRule } from 'lanekeep'
import { resolveImport } from 'lanekeep/paths'

/**
 * Import cycles.
 *
 * A cycle is the architectural failure that hides best. Everything compiles, the bundler
 * copes, and then one module observes a half-initialized binding from another and produces
 * an error miles from its cause — usually only under a particular import order, which is
 * why it survives review and shows up in production.
 *
 * No single file contains a cycle, so this is a reduce-phase rule by nature.
 *
 * @example
 * ```ts
 * import noCircularImports from 'lanekeep/no-circular-imports'
 *
 * export default defineConfig({
 *   rules: [noCircularImports({ maxDepth: 12 })],
 * })
 * ```
 */
export default function noCircularImports(options) {
  // A cycle spanning more files than this is not reported. Cycles get harder to act on the
  // longer they are, and an unbounded search on a pathological graph is the one way this
  // rule could become the slowest thing in a run.
  const maxDepth = options?.maxDepth ?? 24

  return defineRule({
    id: 'lanekeep/no-circular-imports',
    severity: 'error',

    card: {
      message: 'circular import',
      remediation:
        'break the cycle — extract what both modules need into a third, or invert one dependency',
      examples: {
        bad: "// a.ts imports b.ts, and b.ts imports a.ts",
        good: "// a.ts and b.ts both import shared.ts",
      },
    },

    timeout: 5_000,

    query: `
      (import_statement source: (string) @source) @stmt
      (export_statement source: (string) @source) @stmt
    `,

    check(ctx, m) {
      ctx.emitFact({
        kind: 'edge',
        to: ctx.text(m.source).slice(1, -1),
        line: ctx.line(m.stmt),
        column: ctx.column(m.stmt),
      })
    },

    reduce(ctx) {
      const files = new Set(ctx.files)

      // file -> [{ to, line, column }], in fact order, which is (file, emission) order.
      // Everything downstream inherits that determinism.
      const edges = new Map()
      for (const e of ctx.facts('edge')) {
        const target = resolveImport(e.file, e.to, files)
        if (!target || target === e.file) continue

        if (!edges.has(e.file)) edges.set(e.file, [])
        edges.get(e.file).push({ to: target, line: e.line, column: e.column })
      }

      const cycles = findCycles(edges, maxDepth)

      for (const cycle of cycles) {
        // Reported at the import that closes the cycle — the one edge whose removal breaks
        // it. Reporting at every member would turn one problem into N violations and leave
        // the reader to work out they are the same thing.
        ctx.report(
          { file: cycle.file, line: cycle.line, column: cycle.column },
          `circular import: ${cycle.path.join(' → ')}`,
        )
      }
    },
  })
}

/**
 * Every distinct cycle in the graph, each reported once.
 *
 * Iterative depth-first search rather than recursion: a corpus deep enough to overflow the
 * stack would abort the whole run, and a rule must not be able to do that to a run by being
 * given a large repository.
 */
function findCycles(edges, maxDepth) {
  const found = []
  const seen = new Set()

  // Sorted, so the search visits roots in the same order regardless of how the fact list
  // happened to group. Fact order already gives that, but this does not depend on it.
  const roots = [...edges.keys()].sort()

  for (const root of roots) {
    // Each frame: the file, and how far into its edge list we have walked.
    const stack = [{ file: root, index: 0 }]
    const onPath = new Map([[root, 0]])

    while (stack.length > 0) {
      const frame = stack[stack.length - 1]
      const outgoing = edges.get(frame.file) ?? []

      if (frame.index >= outgoing.length) {
        stack.pop()
        onPath.delete(frame.file)
        continue
      }

      const edge = outgoing[frame.index]
      frame.index += 1

      const at = onPath.get(edge.to)
      if (at !== undefined) {
        // This edge closes a cycle: everything on the path from `edge.to` onward.
        const path = stack.slice(at).map((f) => f.file)
        const key = canonical(path)
        if (!seen.has(key)) {
          seen.add(key)
          found.push({
            file: frame.file,
            line: edge.line,
            column: edge.column,
            path: [...path, edge.to],
          })
        }
        continue
      }

      if (stack.length >= maxDepth) continue

      stack.push({ file: edge.to, index: 0 })
      onPath.set(edge.to, stack.length - 1)
    }
  }

  // Sorted by where the violation lands, so two runs over the same corpus report the same
  // cycles in the same order even if the search ever changes.
  found.sort(
    (a, b) => compare(a.file, b.file) || a.line - b.line || a.column - b.column,
  )
  return found
}

/**
 * A cycle's identity, independent of which member the search entered from.
 *
 * `a → b → a` and `b → a → b` are one cycle. Without this the same problem is reported once
 * per member, which is both noise and a different count on a graph that has not changed.
 */
function canonical(path) {
  let best = null
  for (let i = 0; i < path.length; i += 1) {
    const rotation = [...path.slice(i), ...path.slice(0, i)].join('\u0000')
    if (best === null || rotation < best) best = rotation
  }
  return best ?? ''
}

/** String ordering that does not depend on a locale the sandbox does not have. */
function compare(a, b) {
  if (a < b) return -1
  return a > b ? 1 : 0
}
