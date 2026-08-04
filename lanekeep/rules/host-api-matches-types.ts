import { defineRule } from 'lanekeep'
import { inTestCode } from '../modules/rust'

/**
 * The host API and its published TypeScript definitions disagreeing.
 *
 * §3: "The definitions are asserted against this crate's own registration [...] in both
 * directions. A definition that drifts from the engine is worse than none — it produces
 * confident autocomplete for a method that throws at run time." The reverse is quieter and
 * still costly: a host function nobody declares works and is invisible to anyone writing a
 * rule.
 *
 * This replaces two tests that scanned `host.rs` for the substring `object.set("` and split
 * the file on `#[cfg(test)]` to skip fixtures. The registration half is now matched on the
 * AST and skips test code structurally.
 *
 * The definitions half is still a text scan, and honestly so: a rule's query compiles once per
 * language it declares, so no single query matches both Rust call expressions and TypeScript
 * interface members, and §4 gives a rule access only to its own facts — which rules out
 * splitting the work across two rules that meet in a reduce phase. The fragile half moved
 * from the test into the rule; it did not disappear.
 *
 * `packages/lanekeep/index.d.ts` is read through `ctx.readFile`, so it is recorded as a cache
 * dependency and editing either file invalidates this entry.
 */

/**
 * Registered names that are not context members.
 *
 * `report` builds a location object with the same `object.set` call, so these are the
 * violation's fields rather than anything an author reaches. Named individually rather than
 * filtered by heuristic, so a genuinely new host function cannot hide among them.
 */
const NOT_CONTEXT_MEMBERS = ['file', 'loc']

const INTERFACES = ['export interface RuleContext {', 'export interface ReduceContext {']

export default function hostApiMatchesTypes(options) {
  const hostPath = options?.hostPath ?? 'crates/lanekeep-js/src/host.rs'
  const typesPath = options?.typesPath ?? 'packages/lanekeep/index.d.ts'

  return defineRule({
    id: 'local/host-api-matches-types',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'the host API and its published types disagree',
      remediation: 'register the function and declare it in the same change — and bump `host_api_version`, which is a cache key input',
      examples: {
        bad: 'object.set("newThing", ...) with no entry in index.d.ts',
        good: 'both, together, with the version bumped',
      },
    },

    // One match per file: the whole reconciliation is a single invocation, because a
    // comparison of two sets cannot be made one call at a time.
    query: '(source_file) @root',

    check(ctx, m) {
      if (ctx.filePath !== hostPath) return

      const types = ctx.readFile(typesPath)
      if (types === undefined) {
        ctx.report(m.root, {
          message: `\`${typesPath}\` is missing, so the host API has no published types to agree with`,
        })
        return
      }

      const registered = new Set<string>()
      for (const hit of ctx.querySubtree(
        m.root,
        '(call_expression function: (field_expression field: (field_identifier) @method) arguments: (arguments . (string_literal) @arg)) @call',
      )) {
        if (ctx.text(hit.method) !== 'set') continue
        if (inTestCode(ctx, hit.call)) continue
        // A Rust string literal carries its quotes, and a raw string carries `r` and any
        // number of `#`. Stripping a leading `r` unconditionally would eat the first letter
        // of `readFile`, `report` and `root`.
        registered.add(ctx.text(hit.arg).replace(/^r?#*"/, '').replace(/"#*$/, ''))
      }

      const declared = declaredMembers(types)

      for (const name of [...declared].sort()) {
        if (registered.has(name)) continue
        ctx.report(m.root, {
          message: `index.d.ts declares \`${name}\`, which host.rs does not register — autocomplete for a method that throws`,
        })
      }

      for (const name of [...registered].sort()) {
        if (declared.has(name)) continue
        if (NOT_CONTEXT_MEMBERS.includes(name)) continue
        ctx.report(m.root, {
          message: `host.rs registers \`${name}\`, which packages/lanekeep/index.d.ts does not declare — it works but nobody can find it`,
        })
      }
    },
  })
}

/** Members declared on the two context interfaces: a line reading `name(`, `name:` or `name?(`. */
function declaredMembers(types: string): Set<string> {
  const names = new Set<string>()

  for (const header of INTERFACES) {
    const start = types.indexOf(header)
    if (start < 0) continue

    const body = types.slice(start + header.length)
    const end = body.indexOf('\n}')

    for (const raw of body.slice(0, end < 0 ? undefined : end).split('\n')) {
      let line = raw.trim()
      if (line === '' || line.startsWith('//') || line.startsWith('*') || line.startsWith('/*')) continue
      if (line.startsWith('readonly ')) line = line.slice('readonly '.length)

      const matched = /^[A-Za-z0-9_]+/.exec(line)
      if (matched === null) continue

      const name = matched[0]
      const after = line.slice(name.length)
      if (after.startsWith('(') || after.startsWith(':') || after.startsWith('?(')) names.add(name)
    }
  }

  return names
}
