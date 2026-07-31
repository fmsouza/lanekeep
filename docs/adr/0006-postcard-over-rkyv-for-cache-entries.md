# 0006 — postcard over rkyv for cache serialization

**Status:** Accepted, revisit at M1
**Date:** 2026-07-31

## Context

The warm-run budget is under 25 ms for roughly 2,000 files. At that scale, deserializing every
cache entry is a meaningful share of the budget rather than a rounding error.

`rkyv` avoids deserialization entirely: archived data is read in place from the memory-mapped
file. That is close to ideal for this access pattern. It also brings a heavy dependency, a large
amount of `unsafe`, and validation requirements that must be handled correctly or the format
becomes a memory-safety hazard on untrusted input — and a cache file on disk is exactly the kind
of input an attacker who already has filesystem access can shape.

`postcard` with `serde` is small, `#![no_std]`-capable, contains no `unsafe` in its hot path, and
produces compact output. It does require a real deserialization step.

## Decision

`postcard` + `serde` for cache entries. Revisit at M1 when the benchmark suite can answer the
question with a measurement.

## Rationale

The performance claim in favour of `rkyv` is plausible but unmeasured, and the cost is certain:
more dependency surface and more `unsafe` in a tool whose security posture (§11) is built on
having very little of either. Taking a certain cost to buy a speculative benefit is the wrong
order. The benchmark suite lands in the same milestone as the cache, so the measurement arrives
almost immediately.

## Consequences

- Cache reads pay a deserialization cost that may prove material against the 25 ms budget.
- The serialization format sits behind the cache store's interface, so switching is contained.
- If the budget is missed, the response is evidence-driven: profile first, and prefer narrowing
  what gets deserialized before reaching for a zero-copy format.
- Should `rkyv` be adopted, its validation feature is mandatory, not optional — cache files are
  attacker-shapeable and the format must never trust them. The existing "any read error means
  cold cache" policy already provides the failure path.
