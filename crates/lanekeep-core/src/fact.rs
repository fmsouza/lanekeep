//! Facts: what a per-file pass hands to the reduce phase.
//!
//! A cross-file rule cannot see the whole corpus during the per-file pass — nothing does,
//! because files are checked in parallel and independently. What it can do is emit small
//! serializable observations, which the engine collects, orders, and hands back in `reduce`.
//!
//! # Why facts rather than trees
//!
//! Invariant 1 in [`AGENTS.md`]: the reduce phase never touches parse trees. Handing a tree
//! to `reduce` would make the whole corpus resident at once and destroy incrementality —
//! every cross-file rule would force every file to be parsed on every run, which is the
//! cost the cache exists to avoid.
//!
//! Facts are the price of that. They are small, they are JSON, and they can sit in a cache
//! entry beside the violations for the file that produced them. A warm run that reparses
//! nothing can still run `reduce` over the full corpus, because the facts came back from
//! the cache.
//!
//! # Why the payload is a JSON string
//!
//! A fact is whatever shape its rule chose. Modeling that in Rust would mean either a
//! dynamic value type this crate does not otherwise need, or a schema the engine would have
//! to police — and policing it would make fact shape part of lanekeep's public API rather
//! than the rule's private business.
//!
//! Storing the serialized form instead makes caching trivial and keeps the engine
//! uninterested in what a fact means.
//!
//! [`AGENTS.md`]: https://github.com/fmsouza/lanekeep/blob/main/AGENTS.md

use crate::location::FilePath;
use crate::rule_id::RuleId;

/// One observation a rule emitted while checking one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// Which rule emitted it.
    ///
    /// A rule sees only its own facts. Letting one rule read another's would make an
    /// internal payload shape into a contract between rules, and would make the result
    /// depend on the order rules were declared in.
    pub rule_id: RuleId,

    /// The file being checked when it was emitted.
    ///
    /// Set by the host, not by the rule. A rule that puts its own `file` in the payload
    /// does not get to change where the engine thinks the fact came from.
    pub file: FilePath,

    /// The fact's `kind`, lifted out of the payload so `ctx.facts('export')` can filter
    /// without parsing every fact in the corpus.
    pub kind: String,

    /// The payload, as JSON. Always a JSON object.
    pub data: String,

    /// Position among the facts this rule emitted for this file, from zero.
    ///
    /// Emission order within a file is the rule's own doing and is worth preserving —
    /// but it only orders facts *within* a file. Across files, the path breaks the tie.
    pub sequence: u32,
}

/// Sort facts into the order `reduce` will always see them in.
///
/// `(rule, file, sequence)`. Files are checked in parallel, so the order facts arrive in is
/// whatever the thread pool did that run. A `reduce` that iterates facts and reports as it
/// goes would otherwise produce violations in a different order on every run — and while
/// the final violation sort would hide that for output, it would not hide it from a rule
/// that stops at the first match, or counts, or builds a "first seen wins" map.
///
/// Determinism has to hold for what the rule observes, not only for what gets printed.
pub fn sort(facts: &mut [Fact]) {
    facts.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.sequence.cmp(&b.sequence))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(rule: &str, file: &str, sequence: u32) -> Fact {
        Fact {
            rule_id: rule.parse().expect("valid id"),
            file: FilePath::new(file),
            kind: "export".to_owned(),
            data: format!(r#"{{"kind":"export","n":{sequence}}}"#),
            sequence,
        }
    }

    fn order(facts: &[Fact]) -> Vec<(String, String, u32)> {
        facts
            .iter()
            .map(|f| {
                (
                    f.rule_id.to_string(),
                    f.file.as_str().to_owned(),
                    f.sequence,
                )
            })
            .collect()
    }

    #[test]
    fn orders_by_rule_then_file_then_sequence() {
        let mut facts = vec![
            fact("local/b", "a.ts", 0),
            fact("local/a", "b.ts", 1),
            fact("local/a", "a.ts", 1),
            fact("local/a", "b.ts", 0),
            fact("local/a", "a.ts", 0),
        ];
        sort(&mut facts);

        assert_eq!(
            order(&facts),
            vec![
                ("local/a".to_owned(), "a.ts".to_owned(), 0),
                ("local/a".to_owned(), "a.ts".to_owned(), 1),
                ("local/a".to_owned(), "b.ts".to_owned(), 0),
                ("local/a".to_owned(), "b.ts".to_owned(), 1),
                ("local/b".to_owned(), "a.ts".to_owned(), 0),
            ]
        );
    }

    #[test]
    fn emission_order_within_a_file_survives() {
        // A rule that emits `import` then `export` for the same statement is entitled to
        // see them in that order — it is the only ordering it can have intended.
        let mut facts = vec![fact("local/a", "a.ts", 2), fact("local/a", "a.ts", 0)];
        sort(&mut facts);
        assert_eq!(
            facts.iter().map(|f| f.sequence).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[test]
    fn the_order_does_not_depend_on_the_order_facts_arrived_in() {
        // The property that matters: whatever the thread pool did, `reduce` sees one order.
        let canonical = {
            let mut facts = vec![
                fact("local/a", "a.ts", 0),
                fact("local/a", "b.ts", 0),
                fact("local/b", "a.ts", 0),
            ];
            sort(&mut facts);
            order(&facts)
        };

        let mut shuffled = vec![
            fact("local/b", "a.ts", 0),
            fact("local/a", "b.ts", 0),
            fact("local/a", "a.ts", 0),
        ];
        sort(&mut shuffled);
        assert_eq!(order(&shuffled), canonical);

        let mut reversed = vec![
            fact("local/a", "b.ts", 0),
            fact("local/b", "a.ts", 0),
            fact("local/a", "a.ts", 0),
        ];
        sort(&mut reversed);
        assert_eq!(order(&reversed), canonical);
    }

    #[test]
    fn sorting_is_stable_for_facts_that_compare_equal() {
        // Two facts a rule emitted with the same sequence cannot be told apart by the key,
        // so their relative order has to come from somewhere stable rather than from the
        // sort. `sort_by` is stable, which is why this holds.
        let mut first = fact("local/a", "a.ts", 0);
        first.data = r#"{"kind":"export","tag":"first"}"#.to_owned();
        let mut second = fact("local/a", "a.ts", 0);
        second.data = r#"{"kind":"export","tag":"second"}"#.to_owned();

        let mut facts = vec![first, second];
        sort(&mut facts);
        assert!(facts[0].data.contains("first"), "stability was lost");
    }
}
