// A rule in a module of its own, so that the entry's import *order* is observable.
//
// Everything else in this fixture is defined in `rule.js`, the same module that imports the
// runtime — so `withhold()` has necessarily run before any of it exists, and none of it can
// tell you whether that was luck. This module is imported after the runtime and evaluated
// after it, which is the shape every generated entry has and the only shape where the ordering
// requirement in `packages/lanekeep/runtime/entry.js` can be wrong.
//
// The window is module scope and nothing else. A rule's `check` and `reduce` run long after
// both modules have been evaluated, so a rule that reads the clock inside a handler sees it
// withheld however the imports were written; a rule that reads it while its own module body is
// running sees whatever was there at that instant. That is why the line below is at the top
// level and the reported string is a constant computed once.
//
// **And a bundler is what makes this worth building rather than reasoning about.** ES modules
// evaluate depth-first in the order their imports are written, but nothing about that survives
// into a component by itself: rolldown flattens every module into one before `wizer` ever runs,
// and what it emits is a single file whose statements execute top to bottom. That the order is
// preserved through the flattening is a property of the bundler, and this fixture is where it
// is checked rather than assumed.
//
// `typeof` throws on nothing, including an identifier that was never declared, so this module
// records what it saw either way rather than failing on one branch and not the other. A test
// that could only fail by *throwing* here would be reporting the wrong thing anyway — the
// finding is the value, not the exception.

const captured = [
  `Date=${typeof Date}`,
  `Math.random=${typeof Math.random}`,
  `fetch=${typeof fetch}`,
  `setTimeout=${typeof setTimeout}`,
].join(' ; ')

/** Report what this module could see while it was being evaluated. */
export default {
  id: 'probe/order',
  severity: 'error',
  card: {
    message: 'order',
    remediation: 'nothing — `order` is a fixture',
    examples: { bad: 'bad', good: 'good' },
  },
  query: '(program) @p',

  check(ctx) {
    ctx.report(ctx.root, captured)
  },
}
