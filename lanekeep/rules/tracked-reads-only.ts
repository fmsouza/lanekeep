import { defineRule } from 'lanekeep'
import { isNestedInPath } from '../modules/rust'

/**
 * Filesystem access inside the sandbox crate, outside the module that records it.
 *
 * §8.2 replaces purity with tracked effects: every read a rule makes is recorded as
 * `deps: [(path, content_hash)]`, and a cache hit additionally requires every recorded
 * dependency to still hash identically. A read that skips that recording produces a cache
 * entry that is correct on every test anyone thinks to write and wrong on the one case that
 * matters — the file changes and nothing invalidates.
 *
 * Two files legitimately touch the filesystem: `files.rs`, which *is* the tracking, and
 * `loader.rs`, which reads rule modules at load time, before any rule runs and therefore
 * before there is an entry to attribute a dependency to.
 */
export default function trackedReadsOnly(options) {
  const scope = options?.scope ?? []
  const allow = options?.allow ?? []

  return defineRule({
    id: 'local/tracked-reads-only',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'filesystem read that records no cache dependency',
      remediation: 'go through `FileAccess` in `files.rs`, which records the read so the entry invalidates when the file changes',
      examples: {
        bad: 'let text = std::fs::read_to_string(path)?;',
        good: 'let text = access.read(path)?;',
      },
    },

    gates: { fileContains: ['fs::'] },

    query: `
      [
        (use_declaration argument: (_) @path)
        (scoped_identifier path: (_) @path)
      ] @site
    `,

    check(ctx, m) {
      if (!scope.some((prefix: string) => ctx.filePath.startsWith(prefix))) return
      if (allow.includes(ctx.filePath)) return
      if (isNestedInPath(ctx, m.site)) return

      const text = ctx.text(m.path)
      if (!/(^|::)fs($|::)/.test(text)) return

      ctx.report(m.site, {
        message: `\`${text}\` reads without recording a dependency, which makes the cache entry for this file unsound`,
      })
    },
  })
}
