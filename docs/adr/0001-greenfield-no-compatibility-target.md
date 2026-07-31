# 0001 — Greenfield: no compatibility target

**Status:** Accepted
**Date:** 2026-07-31

## Context

lanekeep's design was informed by experience with an earlier, unrelated TypeScript tool that
solved a similar problem inside a single codebase. An early framing of this project treated that
tool as a compatibility target: match its output, accept its suppression directives, reproduce its
rule set.

That framing was wrong. lanekeep ships to users who have never run anything like it. There is no
installed base whose comment syntax must keep working and no downstream consumer parsing an
existing JSON schema.

## Decision

lanekeep has no compatibility target. No compatibility mode, no legacy directive aliases, no
output format inherited from another tool, and no rule set defined by reproducing someone else's.

Acceptance criteria are defined against lanekeep's own fixture corpus and snapshot suite.

## Consequences

- The output schema, directive syntax and rule IDs are designed on their merits alone. Notably,
  this is what makes [ADR-0002](0002-namespaced-rule-ids.md) free rather than a trade-off.
- The fixture corpus becomes a purpose-built conformance suite exercising every predicate cost
  class and every reporter, rather than an imitation of some real codebase's shape. It is a better
  test artifact for having no model to imitate.
- The built-in rule catalogue becomes a product decision — see
  [ADR-0004](0004-built-in-rule-catalogue.md) — rather than an inherited list.
- Nothing is validated against a second implementation. Correctness rests entirely on the fixture
  corpus and the property tests, which raises the bar for both.
