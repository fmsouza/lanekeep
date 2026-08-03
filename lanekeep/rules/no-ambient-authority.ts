import { defineRule } from 'lanekeep'
import { isNestedInPath } from '../modules/rust'

/**
 * Network or subprocess reached from the engine.
 *
 * §13 says "no network. Ever, in any mode, with no configuration that enables it." `deny.toml`
 * enforces that against *crates*, and says nothing about `std` — a `TcpStream` needs no
 * dependency at all.
 *
 * Subprocess is a narrower claim and was never absolute: `--since` and `--staged` shell out to
 * git from `crates/lanekeep-core/src/changed.rs`. That is the whole of it, and `allow` states
 * it so a second one fails the gate rather than passing review as easily as the first did.
 */
const FORBIDDEN = ['std::net', 'std::process', 'process::Command', 'TcpStream', 'UdpSocket']

export default function noAmbientAuthority(options) {
  const allow = options?.allow ?? []

  return defineRule({
    id: 'local/no-ambient-authority',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'network or subprocess reached from the engine',
      remediation: 'lanekeep opens no sockets at all, and shells out only from `changed.rs`, for git',
      examples: {
        bad: 'use std::process::Command;',
        good: '// take the value as a parameter from the caller that already has it',
      },
    },

    query: `
      [
        (use_declaration argument: (_) @path)
        (scoped_identifier path: (_) @path)
      ] @site
    `,

    check(ctx, m) {
      if (allow.includes(ctx.filePath)) return
      if (isNestedInPath(ctx, m.site)) return

      const text = ctx.text(m.path)
      for (const forbidden of FORBIDDEN) {
        if (!text.includes(forbidden)) continue
        ctx.report(m.site, {
          message: `\`${text}\` reaches outside this process, which §13 says lanekeep never does`,
        })
        return
      }
    },
  })
}
