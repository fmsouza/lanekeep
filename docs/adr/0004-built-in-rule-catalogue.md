# 0004 — A four-rule built-in catalogue for v0.1

**Status:** Accepted
**Date:** 2026-07-31

## Context

With no inherited rule set ([ADR-0001](0001-greenfield-no-compatibility-target.md)), the built-in
catalogue is a product decision.

Two constraints narrow it. lanekeep is explicitly not for what ESLint already covers, so
language-level rules are out. And the tool's thesis is that meaningful rules are project-specific
and belong in config as `local/*` — a large built-in catalogue would contradict the premise the
whole design rests on.

Against that, the architecture has one part that is unproven and expensive to get wrong: the
reduce phase, which lets cross-file rules consume facts without touching parse trees. Shipping it
unexercised is how it turns out to be inadequate at M4 instead of M0.

## Decision

Four built-in rules for v0.1.

| Rule | Class | Purpose |
| --- | --- | --- |
| `lanekeep/no-default-export` | per-file | Named exports only. Already referenced by §7's config example. |
| `lanekeep/no-restricted-imports` | per-file, parameterized | Forbid given modules from given paths. Validates per-rule options. |
| `lanekeep/no-unused-exports` | cross-file | An exported symbol no module imports. Exercises the facts join. |
| `lanekeep/no-circular-imports` | cross-file | Import cycles. Exercises facts under graph traversal. |

## Rationale

The two per-file rules are chosen for coverage of the config surface rather than ambition:
`no-default-export` is the simplest possible end-to-end rule, and `no-restricted-imports` is the
first rule that needs per-rule options, forcing that part of the config schema to be real.

The two cross-file rules stress the reduce phase along different axes. A join and a graph
traversal have different access patterns, and validating only one leaves the abstraction
half-tested. §14 of the architecture document is explicit that learning an abstraction is wrong
late is the expensive failure mode.

## Consequences

- Users get a usable tool out of the box without the catalogue implying lanekeep is a general
  linter.
- Both non-declarative paths — the `Rule` trait's Rust implementors and the reduce phase — are
  exercised at M0 rather than theoretical.
- Four rules is a thin catalogue for a tool asking to be adopted. Mitigated by shipping example
  `local/*` presets in `fixtures/` and documenting rule authoring properly; new built-ins arrive
  by contribution, which §1 of the architecture already anticipates.
