import { defineRule } from 'lanekeep'
import { resolveImport } from 'lanekeep/paths'

/**
 * Exports nobody in the corpus imports.
 *
 * Dead exported code is worse than dead private code: it is part of a module's surface, so
 * a reader has to assume something depends on it, and an agent will happily wire new code
 * up to it. Nothing in the exporting file reveals the problem — which is exactly why this
 * needs the reduce phase rather than a query.
 *
 * Matching is by `(resolved module, symbol)`, not by symbol name. A rule that treated any
 * `parse` imported anywhere as covering every exported `parse` would go quiet on a codebase
 * with common names, which is every codebase.
 *
 * @example
 * ```ts
 * import noUnusedExports from 'lanekeep/no-unused-exports'
 *
 * export default defineConfig({
 *   rules: [noUnusedExports({ entryPoints: ['src/index.ts', 'src/cli.ts'] })],
 * })
 * ```
 */
export default function noUnusedExports(options) {
  const entryPoints = options?.entryPoints ?? []

  return defineRule({
    id: 'lanekeep/no-unused-exports',
    severity: 'error',

    card: {
      message: 'unused export',
      remediation:
        'stop exporting it, or delete it — an export nobody imports is surface with no users',
      examples: {
        bad: 'export function helper() {}',
        good: 'function helper() {}',
      },
    },

    // The whole corpus feeds one pass, so the default per-invocation budget is the wrong
    // shape for it. See docs/cross-file-rules.md.
    timeout: 5_000,

    query: `
      (export_statement
        declaration: (function_declaration name: (identifier) @name)) @stmt

      (export_statement
        declaration: (class_declaration name: (type_identifier) @name)) @stmt

      (export_statement
        declaration: (lexical_declaration
          (variable_declarator name: (identifier) @name))) @stmt

      (export_statement
        (export_clause
          (export_specifier !alias name: (identifier) @name))) @stmt

      (export_statement
        (export_clause
          (export_specifier alias: (identifier) @name))) @stmt

      ; Children match in tree order, and \`import_clause\` precedes \`source\` in the
      ; grammar — the other way round is a pattern that can never match.
      ; The *name*, aliased or not: \`import { x as y }\` consumes \`x\` from the target.
      ; Capturing the alias would leave \`x\` looking unused and report it.
      (import_statement
        (import_clause (named_imports (import_specifier name: (identifier) @imported)))
        source: (string) @source)

      (import_statement
        (import_clause (namespace_import))
        source: (string) @source) @wildcard

      (export_statement source: (string) @source) @wildcard
    `,

    check(ctx, m) {
      if (m.wildcard) {
        // `import * as ns from './a'` and `export * from './a'` both consume the whole
        // module, and neither names what it takes. Treating them as covering everything in
        // the target is the only sound reading — the alternative is reporting exports that
        // are demonstrably reachable.
        ctx.emitFact({ kind: 'wildcard', from: unquote(ctx.text(m.source)) })
        return
      }

      if (m.imported) {
        ctx.emitFact({
          kind: 'import',
          symbol: ctx.text(m.imported),
          from: unquote(ctx.text(m.source)),
        })
        return
      }

      ctx.emitFact({
        kind: 'export',
        symbol: ctx.text(m.name),
        line: ctx.line(m.stmt),
        column: ctx.column(m.stmt),
      })
    },

    reduce(ctx) {
      const files = new Set(ctx.files)

      // Anything a wildcard reaches is used, whatever its name.
      const consumedWhole = new Set()
      for (const w of ctx.facts('wildcard')) {
        const target = resolveImport(w.file, w.from, files)
        if (target) consumedWhole.add(target)
      }

      // Keyed `${file}\u0000${symbol}`. A NUL separator rather than a space, because a
      // path may contain spaces and `a b` + `c` would then key the same as `a` + `b c`.
      // Written as an escape: a raw NUL in source is invisible in an editor and makes
      // the parser report a failure lines away from the character that caused it.
      const used = new Set()
      for (const i of ctx.facts('import')) {
        const target = resolveImport(i.file, i.from, files)
        if (target) used.add(`${target}\u0000${i.symbol}`)
      }

      for (const e of ctx.facts('export')) {
        // An entry point is exported for something outside the corpus — a bin, a package
        // main, a test runner. Without this the rule reports every public API of a library
        // as unused, which is true of the corpus and useless as advice.
        if (entryPoints.includes(e.file)) continue
        if (consumedWhole.has(e.file)) continue
        if (used.has(`${e.file}\u0000${e.symbol}`)) continue

        ctx.report(
          { file: e.file, line: e.line, column: e.column },
          `'${e.symbol}' is exported but nothing imports it`,
        )
      }
    },
  })
}

/** Strip the quotes from a string literal as it appears in source. */
function unquote(raw) {
  return raw.slice(1, -1)
}
