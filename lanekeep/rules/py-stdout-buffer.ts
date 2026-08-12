import { defineRule } from 'lanekeep'

/**
 * Text written to `sys.stdout` rather than to `sys.stdout.buffer`.
 *
 * Two distinct Windows failures meet here. `sys.stdout` encodes with the locale codec, which
 * is cp1252, so writing any text carrying an em dash dies with `UnicodeEncodeError` partway
 * through — the output is *truncated at the first non-ASCII character* rather than mangled,
 * which is what makes it quiet. And `sys.stdout` translates newlines, so a value read back by
 * a shell script arrives carrying a carriage return; two such values still compare equal to
 * each other, so only a comparison against a literal fails, and everything downstream of that
 * silently stops happening.
 *
 * `sys.stdout.buffer.write` of raw bytes avoids both at once.
 */
export default defineRule({
  id: 'local/py-stdout-buffer',
  language: 'python',
  severity: 'error',

  card: {
    message: 'text written to sys.stdout',
    remediation: 'write bytes through `sys.stdout.buffer.write`, which neither re-encodes nor translates newlines',
    examples: {
      bad: 'sys.stdout.write(text)',
      good: 'sys.stdout.buffer.write(text.encode("utf-8"))',
    },
  },

  gates: { fileContains: ['sys.stdout'] },

  query: '(call function: (attribute object: (_) @obj attribute: (identifier) @method)) @call',

  check(ctx, m) {
    if (ctx.text(m.method) !== 'write') return
    if (ctx.text(m.obj) !== 'sys.stdout') return

    ctx.report(m.call, {
      message: 'sys.stdout encodes with the locale codec, which on Windows truncates the output at the first non-ASCII byte',
    })
  },
})
