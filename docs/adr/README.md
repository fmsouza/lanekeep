# Architecture decision records

Each record captures one decision: the context that forced it, what was chosen, and what the
choice costs. Records are immutable once accepted — a decision that changes gets a new record
that supersedes the old one, rather than an edit that erases the reasoning.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-greenfield-no-compatibility-target.md) | Greenfield: no compatibility target | Accepted |
| [0002](0002-namespaced-rule-ids.md) | Namespaced rule IDs from day one | Accepted |
| [0003](0003-tree-sitter-queries-over-gritql.md) | tree-sitter queries as the tier-1 query language | Accepted |
| [0004](0004-built-in-rule-catalogue.md) | A four-rule built-in catalogue for v0.1 | Accepted |
| [0005](0005-licensing-and-distribution.md) | Dual licensing, public development, registry publishing | Accepted |
| [0006](0006-postcard-over-rkyv-for-cache-entries.md) | postcard over rkyv for cache serialization | Accepted, revisit at M1 |

## Writing a new record

Copy the structure of an existing one. Keep it short: context, decision, consequences, and — where
the decision closes off alternatives — what it would take to reverse. A record nobody reads because
it is long has failed at its only job.
