<!--
The title becomes the commit message on main (squash merge) and release-plz reads it to
pick the next version. Conventional Commits — feat, fix, docs, style, refactor, perf,
test, build, ci, chore, revert. Use `!` for a breaking change.
-->

## What

<!-- What changes, in a sentence or two. -->

## Why

<!--
The part reviewers cannot reconstruct from the diff. What problem does this solve, and
why this approach over the alternative you rejected?
-->

## Trade-off

<!--
Every real change costs something. Naming the cost yourself is faster than having it
found. Delete this section only if the change genuinely has no downside.
-->

## Verification

<!--
What you actually ran, and what it said. Not what should pass — what did.
-->

## Invariants

<!--
Tick anything this change touches. Each is somewhere a mistake stays quiet: it passes
CI, merges, and surfaces later as a stale result or a nondeterministic diff. If any box
is ticked, say in "Why" how the invariant is preserved.

See AGENTS.md and docs/architecture.md §14.
-->

- [ ] Determinism — output ordering, or anything observing time, randomness, environment
      or iteration order
- [ ] Cache soundness — a new input affecting results, or a change to what is hashed
- [ ] Reduce phase — anything crossing into `reduce` beyond facts and the file list
- [ ] Sandbox boundary — new host API surface, or new capability reachable by rule code
- [ ] Boundary crossings — a change to how often JavaScript is invoked per file
- [ ] Resource limits — timeouts, memory ceiling, or cancellation behavior
- [ ] None of the above
