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
 * git from `crates/lanekeep-core/src/changed.rs`. That is the whole of it for the engine, and
 * `allow` states it so a second one fails the gate rather than passing review as easily as the
 * first did.
 *
 * Integration tests under `tests/` are a different case, not a second exemption for the same
 * thing: they spawn the compiled binary, or shell out to git to build fixtures, which is the
 * scaffolding that proves the engine works rather than the engine reaching outside itself
 * during a run. Excluded the same way `no-unwrap` excludes them, so the two rules agree on
 * what "a test" means.
 *
 * `std::process` alone is not in this list. It would also match `std::process::ExitCode`,
 * which is how a process reports its own exit status and reaches nothing outside it —
 * `process::Command` is the capability this rule is actually about, and it matches both
 * `std::process::Command` and an already-imported bare `process::Command`.
 */
const FORBIDDEN = ['std::net', 'process::Command', 'TcpStream', 'UdpSocket']

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
      const path = ctx.filePath
      if (allow.includes(path)) return

      // Integration tests spawn the compiled binary, and the CLI corpus helper shells out to
      // git to build fixtures. §13's claim is about what the engine does in a run, not about
      // the scaffolding that proves it works — the same trade `no-unwrap` makes, and the same
      // spelling, so the two rules agree on what "a test" means.
      if (path.includes('/tests/') || path.startsWith('tests/')) return

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
