# 0002 — Namespaced rule IDs from day one

**Status:** Accepted
**Date:** 2026-07-31

## Context

Rule identifiers appear in four places that are expensive to change once users exist: config
files, suppression comments in source, JSON output consumed by other tools, and CI configuration
that filters on specific rules.

Shipping bare identifiers (`no-default-export`) and adding namespaces later would break all four
simultaneously. The architecture document calls this out as the most expensive of its one-way
doors.

## Decision

Rule IDs are namespaced from the first release:

- `lanekeep/<id>` — built-in rules, reviewed and shipped by maintainers.
- `local/<id>` — rules authored in a project's own config.

Suppression directives are `lanekeep-ignore-next-line` and `lanekeep-ignore-file`, each requiring
one or more rule IDs and a mandatory `reason:`.

## Consequences

- Config authors cannot accidentally shadow a built-in rule, because the namespaces are disjoint.
- Output is unambiguous about a rule's provenance, which matters when a violation's remediation
  depends on whether a maintainer or a teammate wrote the rule.
- Future rule sources — a plugin system, shared preset packages — extend the namespace set instead
  of changing the identifier grammar.
- Identifiers are slightly longer to type in suppression comments. Accepted.
