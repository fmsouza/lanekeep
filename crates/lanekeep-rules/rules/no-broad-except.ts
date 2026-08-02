import { defineRule } from 'lanekeep'

/**
 * Catching everything in Python.
 *
 * A bare `except:` catches `KeyboardInterrupt` and `SystemExit` too, so it swallows the
 * user pressing Ctrl-C. `except Exception:` is narrower and still catches every bug in the
 * block — a typo'd attribute, a `None` where an object was expected — and reports whatever
 * the handler decided the failure was.
 *
 * The resolver is what makes this precise rather than a text match. A project that defines
 * its own `Exception` class, or imports one, is not catching the builtin, and firing there
 * would be wrong. `ctx.bindingKind` answers that: a name with any local binding is not the
 * builtin.
 *
 * @example
 * ```ts
 * import noBroadExcept from 'lanekeep/no-broad-except'
 *
 * export default defineConfig({ rules: [noBroadExcept] })
 * ```
 */
export default defineRule({
  id: 'lanekeep/no-broad-except',
  language: 'python',
  severity: 'error',

  card: {
    message: 'except catches too much',
    remediation:
      'name the exceptions this block can actually raise, so an unexpected one still surfaces',
    examples: {
      bad: 'try:\n    parse(raw)\nexcept Exception:\n    return None',
      good: 'try:\n    parse(raw)\nexcept ValueError:\n    return None',
    },
  },

  gates: { fileContains: ['except'] },

  query: '(except_clause) @clause',

  check(ctx, m) {
    const children = ctx.namedChildren(m.clause)

    // `except:` — no exception named at all. The first named child of a bare clause is its
    // block, because `except` itself is an anonymous token.
    const caught = children[0]
    if (!caught || ctx.kind(caught) === 'block') {
      ctx.report(m.clause, {
        message: 'bare `except:` also catches KeyboardInterrupt and SystemExit',
      })
      return
    }

    // `except Exception:` and `except Exception as e:`, which wraps the name in a pattern.
    const named =
      ctx.kind(caught) === 'as_pattern' ? ctx.namedChildren(caught)[0] : caught
    if (!named || ctx.kind(named) !== 'identifier') return

    const name = ctx.text(named)
    if (name !== 'Exception' && name !== 'BaseException') return

    // Locally defined or imported under that name — a different exception entirely.
    if (ctx.bindingKind(named)) return

    ctx.report(m.clause, {
      message: `\`except ${name}\` catches every error the block can raise, including bugs`,
    })
  },
})
