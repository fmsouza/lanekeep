import { defineRule } from 'lanekeep'

/**
 * A binding kind a resolver can return that the `BindingKind` union does not include.
 *
 * `ctx.bindingKind` is typed as a union rather than as `string`, which is only useful while
 * the union is complete. A language crate adding a kind without adding it here narrows an
 * author's `switch` to something wrong — silently, since the missing arm just never matches.
 *
 * The kinds are read from `as_str`'s match arms rather than from a list, for the same reason
 * §3 reads the host surface from `host.rs`: the compiler already forces that match to be
 * exhaustive, so it is the one place that cannot fall behind the enum. Four languages have
 * each added kinds to it — `assignment` and `comprehension` for Python, `receiver` and
 * `type-param` for Go, `module` and `trait` for Rust — on the test that the nearest existing
 * kind would have been untrue.
 */
export default function bindingKindsAreTyped(options) {
  const bindingPath = options?.bindingPath ?? 'crates/lanekeep-lang/src/binding.rs'
  const typesPath = options?.typesPath ?? 'packages/lanekeep/index.d.ts'

  return defineRule({
    id: 'local/binding-kinds-are-typed',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'binding kind missing from the BindingKind union',
      remediation: "add the kind to `BindingKind` in packages/lanekeep/index.d.ts — an author's switch on a narrowed union silently never matches",
      examples: {
        bad: "Self::Trait => \"trait\" with no 'trait' in the union",
        good: "both, in the same change",
      },
    },

    query: '(source_file) @root',

    check(ctx, m) {
      if (ctx.filePath !== bindingPath) return

      const types = ctx.readFile(typesPath)
      if (types === undefined) {
        ctx.report(m.root, {
          message: `\`${typesPath}\` is missing, so the binding kinds have no union to agree with`,
        })
        return
      }

      const union = unionMembers(types)

      // The string on the right of a match arm inside `as_str`. Reading the arm rather than
      // the enum variant, because `as_str` is what a resolver's answer travels through.
      for (const hit of ctx.querySubtree(
        m.root,
        '(match_arm value: (string_literal) @kind) @arm',
      )) {
        const kind = ctx.text(hit.kind).replace(/^r?#*"/, '').replace(/"#*$/, '')
        if (kind === '') continue
        if (union.has(kind)) continue

        ctx.report(hit.arm, {
          message: `\`${kind}\` is a binding kind the resolvers can return, and BindingKind does not include it — an author's switch silently never matches it`,
        })
      }
    },
  })
}

/** The string literals in the `BindingKind` union type. */
function unionMembers(types: string): Set<string> {
  const members = new Set<string>()

  const start = types.indexOf('export type BindingKind')
  if (start < 0) return members

  const body = types.slice(start)
  const end = body.indexOf('\n\n')
  const declaration = body.slice(0, end < 0 ? undefined : end)

  for (const quoted of declaration.match(/'[^']+'/g) ?? []) {
    members.add(quoted.slice(1, -1))
  }

  return members
}
