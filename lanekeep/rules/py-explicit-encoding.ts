import { defineRule } from 'lanekeep'

/**
 * A text file opened without naming its encoding.
 *
 * Python's default text encoding is locale-dependent, and on Windows that is cp1252 rather
 * than UTF-8. This repository's prose is full of em dashes, so a helper reading a file that
 * embeds the README — a wheel's METADATA, for one — dies with `UnicodeEncodeError` partway
 * through on Windows and nowhere else.
 *
 * AGENTS.md records the shipped instance and its remedy: "Reading is already safe as long as
 * every `read_text`/`open` names `encoding="utf-8"`, which they must." This rule is what makes
 * "which they must" true.
 */
const NEEDS_ENCODING = ['open', 'read_text', 'write_text']

export default defineRule({
  id: 'local/py-explicit-encoding',
  language: 'python',
  severity: 'error',

  card: {
    message: 'text file opened without an explicit encoding',
    remediation: 'pass `encoding="utf-8"` — the default is locale-dependent, and on Windows it is cp1252',
    examples: {
      bad: 'text = path.read_text()',
      good: 'text = path.read_text(encoding="utf-8")',
    },
  },

  query: `
    [
      (call function: (identifier) @fn arguments: (argument_list) @args)
      (call function: (attribute attribute: (identifier) @fn) arguments: (argument_list) @args)
    ] @call
  `,

  check(ctx, m) {
    const fn = ctx.text(m.fn)
    if (!NEEDS_ENCODING.includes(fn)) return

    // A binary `open` takes no encoding at all — passing one raises `ValueError: binary mode
    // doesn't take an encoding argument`, so reporting it would send an author to a change
    // that breaks the script. `read_text`/`write_text` have no mode; `read_bytes` is the
    // binary spelling and is not on the list.
    if (fn === 'open' && isBinaryMode(ctx, m.args)) return

    for (const arg of ctx.namedChildren(m.args)) {
      if (ctx.kind(arg) !== 'keyword_argument') continue
      if (ctx.text(arg).startsWith('encoding')) return
    }

    ctx.report(m.call, {
      message: `\`${fn}\` without \`encoding=\` reads cp1252 on Windows, which fails on the first non-ASCII byte`,
    })
  },
})

/**
 * Whether an `open` call asks for binary mode.
 *
 * Mode is the second positional argument or a `mode=` keyword. Only the mode is inspected —
 * a path that happens to contain a `b` is not a mode.
 */
function isBinaryMode(ctx: any, args: any): boolean {
  let positional = 0

  for (const arg of ctx.namedChildren(args)) {
    const kind = ctx.kind(arg)

    if (kind === 'keyword_argument') {
      if (!ctx.text(arg).startsWith('mode')) continue

      // The literal, not the whole `mode=...` text. `mode=readable_mode` contains a `b` in
      // the identifier's spelling, and treating that as binary would silently exempt a call
      // that genuinely needs an encoding — the exact silencing this guard must not do. Same
      // discipline as the positional branch below: prove it is a string before reading it.
      const parts = ctx.namedChildren(arg)
      const value = parts[parts.length - 1]
      return ctx.kind(value) === 'string' && ctx.text(value).includes('b')
    }

    positional += 1
    // First positional is the path, second is the mode.
    if (positional === 2) return kind === 'string' && ctx.text(arg).includes('b')
  }

  return false
}
