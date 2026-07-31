# 0003 — tree-sitter queries as the tier-1 query language

**Status:** Accepted
**Date:** 2026-07-31

## Context

§15 of the architecture document left one decision explicitly open, and identified it as the one
deserving a spike rather than a judgement call: whether rules are written as tree-sitter
S-expression queries or in GritQL.

The stakes are unusual because lanekeep has no plugin escape hatch. A user who cannot express a
rule cannot drop into code — the only remedies are a new built-in predicate or a new built-in
rule, both requiring an upstream pull request. The query language therefore sets the tool's
expressiveness ceiling.

GritQL is Rust-native, more expressive, has better scope and negation handling than raw
tree-sitter queries, and Biome has already adopted it.

## Decision

tree-sitter S-expression queries. GritQL is not adopted for v1.

## Rationale

- GritQL's principal advantage is its rewrite operator, which serves autofix. lanekeep's autofix
  design is template-based replacement of a named capture, which does not need it.
- Biome's GritQL plugin is diagnostic-only; rewrite support is planned rather than shipped. The
  most mature adopter has not yet realised the advantage being paid for.
- GritQL is built on tree-sitter. Adopting it does not remove tree-sitter from the dependency
  graph — it adds a substantial layer above it, against §11's minimal-surface posture for a tool
  designed to run as a pre-commit hook.
- The C3 (structural) and C4 (binding) predicate classes exist precisely to cover tree-sitter
  queries' weakness at negation and scope. That work is planned regardless, and it is reusable
  across languages in a way a query-language choice is not.

## Consequences

- Complexity is absorbed into predicates, which are ordinary Rust functions addable without
  touching any grammar. The query surface stays small and stable.
- The expressiveness ceiling is lower than GritQL's. When a rule cannot be expressed, the correct
  responses are a new built-in predicate or a built-in Rust rule — never a new construct in the
  config language. This line is load-bearing; see §6 of the architecture document.
- Rule authors write S-expressions, which are less approachable than GritQL's pattern syntax. The
  rule-authoring playbook and `RuleTester` carry the weight of making this tractable.

## Reversibility

Additive. A second query compiler behind the existing `query:` field, selected by a per-rule
dialect marker, adds GritQL without invalidating any existing rule. The trigger would be a
genuine expressiveness failure where the answer is not a new predicate — not mere preference.
