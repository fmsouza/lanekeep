import { defineRule } from 'lanekeep'

/**
 * The environment observed on a path whose output is cached.
 *
 * §8.1: everything is deterministic given `(bytes, path, ruleset, config, tracked reads)`.
 * Anything else a result depends on is an input the cache key does not have, so the entry is
 * served for a run that would have computed something different. AGENTS.md states it without
 * the exception: "If you add anything that observes the environment, you have broken the
 * cache. There is no 'it's only used for logging' exception; a cached result is a cached
 * result."
 *
 * Two sites legitimately read the clock, and `allow` names both so the distinction is
 * reviewable rather than assumed:
 *
 * - `suppression::today`, which a suppression's `expires:` has to be compared against. Fixed
 *   once per run, in UTC, so two files checked a millisecond apart cannot disagree.
 * - `RunClock::start` in `lanekeep-js`, which fixes the origin the global run budget is
 *   measured from. A timeout clock cannot change *what* gets cached: §6.8 cancels the whole
 *   run on a breach — exit `2`, no report, no entry for any file still in flight — so it can
 *   stop a result being produced but cannot produce a different one.
 *
 * Anything else reading the clock is an input the cache key does not have.
 */
const FORBIDDEN = [
  'SystemTime::now',
  'Instant::now',
  'env::var',
  'env::vars',
  'env::current_dir',
  // Qualified, like every other entry, because the match is `callee.endsWith(forbidden)` and
  // a bare `'random'` also matches `getrandom::getrandom`. Note the limit: the query matches
  // `(call_expression function: (scoped_identifier))`, so an imported `use rand::thread_rng;`
  // called as plain `thread_rng()` is an `identifier` and never reaches `check`. That is
  // acceptable here because randomness needs a dependency this workspace does not have, and
  // adding one is itself a visible, reviewable act — this rule is the second line, not the only one.
  'rand::thread_rng',
  'rand::random',
]

export default function noAmbientObservation(options) {
  const scope = options?.scope ?? []

  // A rule scoped to nothing checks nothing and reports a clean run, which is the failure
  // this whole rule set exists to prevent. `allow` defaults to empty and that is safe — it
  // makes the rule check *more*. `scope` is the opposite polarity, so an empty one has to be
  // refused rather than defaulted. Loud at config load beats silent forever.
  if (scope.length === 0) {
    throw new Error(
      'local/no-ambient-observation needs a non-empty `scope`; with none it silently checks nothing',
    )
  }
  const allow = options?.allow ?? []

  return defineRule({
    id: 'local/no-ambient-observation',
    language: 'rust',
    severity: 'error',

    card: {
      message: 'the environment observed where the result is cached',
      remediation: 'take the value as a parameter, fixed once per run by the caller, so the cache key can account for it',
      examples: {
        bad: 'let today = SystemTime::now();',
        good: 'fn check(today: Date) { /* the host fixes it once per run */ }',
      },
    },

    query: '(call_expression function: (scoped_identifier) @callee) @call',

    check(ctx, m) {
      if (!scope.some((prefix: string) => ctx.filePath.startsWith(prefix))) return
      if (allow.includes(ctx.filePath)) return

      const callee = ctx.text(m.callee)
      for (const forbidden of FORBIDDEN) {
        if (!callee.endsWith(forbidden)) continue
        ctx.report(m.call, {
          message: `\`${callee}\` is an input the cache key does not have, so an entry computed with it can be served for a run that would compute something else`,
        })
        return
      }
    },
  })
}
