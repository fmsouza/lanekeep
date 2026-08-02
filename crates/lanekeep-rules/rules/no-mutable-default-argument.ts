import { defineRule } from 'lanekeep'

/**
 * Mutable default arguments in Python.
 *
 * `def f(items=[])` evaluates `[]` once, when the function is defined, and every call that
 * does not pass `items` shares that one list. The second call sees what the first appended.
 *
 * It reads as a per-call default and is not one, which is what makes it worth a rule rather
 * than a comment: the code looks right, the bug appears later and somewhere else, and an
 * agent writing Python produces it constantly.
 *
 * @example
 * ```ts
 * import noMutableDefaultArgument from 'lanekeep/no-mutable-default-argument'
 *
 * export default defineConfig({ rules: [noMutableDefaultArgument] })
 * ```
 */
export default defineRule({
  id: 'lanekeep/no-mutable-default-argument',
  language: 'python',
  severity: 'error',

  card: {
    message: 'mutable default argument',
    remediation:
      'default to None and build the value inside the function, so each call gets its own',
    examples: {
      bad: 'def add(item, items=[]):',
      good: 'def add(item, items=None):\n    items = [] if items is None else items',
    },
  },

  // Every default, filtered in the handler. The mutable ones are a small set and the query
  // cannot express "is one of these kinds" without repeating the pattern once per kind.
  query: '(default_parameter value: (_) @value) @param',

  check(ctx, m) {
    const kind = ctx.kind(m.value)

    // A literal container is constructed once, at definition time.
    if (kind === 'list' || kind === 'dictionary' || kind === 'set') {
      ctx.report(m.param, {
        message: `default \`${ctx.text(m.value)}\` is created once and shared by every call`,
      })
      return
    }

    // `list()`, `dict()`, `set()` — the same thing spelled as a call. Only the builtins,
    // and only when nothing local has taken the name: a project that defines its own
    // `list` is not doing this.
    if (kind === 'call') {
      const callee = ctx.namedChildren(m.value)[0]
      if (!callee || ctx.kind(callee) !== 'identifier') return

      const name = ctx.text(callee)
      if (name !== 'list' && name !== 'dict' && name !== 'set') return
      if (ctx.bindingKind(callee)) return

      ctx.report(m.param, {
        message: `default \`${name}()\` is created once and shared by every call`,
      })
    }
  },
})
