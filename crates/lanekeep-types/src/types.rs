//! What the oracle can say about an expression.
//!
//! This crate owns the vocabulary as well as the analysis, because the two move together: a
//! variant added here is a question the oracle must then answer, and a question it cannot
//! answer has no business being spellable.

/// A primitive type the oracle recognizes.
///
/// Exactly the set the authoring surface's `TypeInfo.primitive` names, and no more. `any`
/// and `unknown` are deliberately absent: they are the absence of a claim, and giving them
/// a variant would let the oracle assert something TypeScript does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Primitive {
    /// TypeScript's `number`.
    Number,
    /// TypeScript's `string`.
    String,
    /// TypeScript's `boolean`.
    Boolean,
    /// TypeScript's `bigint`.
    BigInt,
    /// TypeScript's `symbol`.
    Symbol,
    /// TypeScript's `null`.
    Null,
    /// TypeScript's `undefined`.
    Undefined,
}

impl Primitive {
    /// The name a rule sees, which is the name TypeScript uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::BigInt => "bigint",
            Self::Symbol => "symbol",
            Self::Null => "null",
            Self::Undefined => "undefined",
        }
    }
}

/// Where a name came from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol {
    /// The name as it appears at the use site the oracle read, not at the declaration.
    ///
    /// For a renamed import — `import { Decimal as Money }` — this is the local alias
    /// `Money`. It stays the use-site spelling because it is what a message should quote: the
    /// reader has `Money` in front of them, and a violation naming `Decimal` would send them
    /// looking for text that is not in their file. [`Symbol::exported`] is the field to
    /// compare against an expected export name.
    pub name: String,
    /// The name the module exports this under, when it was imported.
    ///
    /// | Binding | `exported` |
    /// | --- | --- |
    /// | `import { a } from 'm'`, `import { a as b } from 'm'` | `Some("a")` |
    /// | `import d from 'm'` | `Some("default")` |
    /// | `import * as ns from 'm'` | `None` |
    /// | a local declaration | `None` |
    ///
    /// **Copied even when nothing was renamed**, rather than `None` standing for "same as
    /// `name`". The consumer is a comparison against a required export name, and under a
    /// `None`-when-unrenamed contract a caller who forgot the `?? name` fallback would
    /// silently accept every plain import — the most ordinary spelling there is, and a
    /// failure that only ever *removes* reports, so nothing about the output would look
    /// wrong. A copy costs one `String` per symbol and cannot fail that way.
    ///
    /// A namespace import binds the module object, which no single export names, so it is
    /// `None` rather than a `"*"` sentinel: a sentinel is a string a comparison can match,
    /// and there is nothing here for a name comparison to be right about.
    ///
    /// `Binding::is_import_of`, the predicate behind `ctx.resolvesToImport`, does spell a
    /// namespace import `"*"`, and the two do not conflict: there `"*"` is what a rule
    /// *writes* to ask for the namespace form, a query vocabulary; here the value is an
    /// *answer* a rule compares against, and an answer of `"*"` would satisfy any comparison
    /// that happened to write it.
    pub exported: Option<String>,
    /// The module it was imported from, when it was imported. `None` for a local
    /// declaration, which is what distinguishes an imported `Decimal` from a local class
    /// that happens to share the name.
    pub module: Option<String>,
}

/// What the oracle established about an expression.
///
/// There is no `Unknown` variant on purpose: not knowing is `None` at the API boundary, so
/// there is exactly one spelling of it. A variant would give callers two, and the
/// interesting bug is a rule that treats one of them as an answer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Type {
    /// A primitive.
    Primitive(Primitive),
    /// A type named by an identifier — `Decimal`, `Account`. Nominal, by name: the oracle
    /// compares names and does not compute structural assignability.
    Nominal {
        /// The name as written.
        name: String,
        /// Where that name came from, when the resolver could say.
        symbol: Option<Symbol>,
    },
    /// Two or more distinct members, flattened one level, in canonical order.
    Union(Vec<Type>),
}

impl Type {
    /// Build a union from its members, or the single type if that is what it comes to.
    ///
    /// Members are flattened one level, deduplicated and sorted. Sorting is not the
    /// determinism requirement — source order is deterministic too — it is that
    /// `string | number` and `number | string` are the same type and must produce one
    /// answer. `Ord` on [`Type`] puts primitives before nominals and orders each group by
    /// name, which is a total order over everything this crate can build.
    ///
    /// Returns `None` for an empty union, which is not a type and must not be reported as
    /// one.
    #[must_use]
    pub fn union(members: Vec<Self>) -> Option<Self> {
        let mut flattened: Vec<Self> = Vec::with_capacity(members.len());
        for member in members {
            match member {
                // One level. A member that is itself a union was already flattened when it
                // was built, so its own members are never unions.
                Self::Union(inner) => flattened.extend(inner),
                other => flattened.push(other),
            }
        }
        flattened.sort();
        flattened.dedup();

        match flattened.len() {
            0 => None,
            1 => flattened.pop(),
            _ => Some(Self::Union(flattened)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_union_of_one_is_that_type() {
        assert_eq!(
            Type::union(vec![Type::Primitive(Primitive::Number)]),
            Some(Type::Primitive(Primitive::Number))
        );
    }

    #[test]
    fn a_union_of_none_is_nothing() {
        assert_eq!(Type::union(Vec::new()), None);
    }

    /// The canonical-order claim, asserted head-on.
    ///
    /// `string | number` and `number | string` are the same type, so they must produce the
    /// same answer. Source order is deterministic too, which is why this is not a
    /// determinism test: it is a correctness one about what a union *is*.
    #[test]
    fn union_members_do_not_depend_on_the_order_they_were_written() {
        let one = Type::union(vec![
            Type::Primitive(Primitive::String),
            Type::Primitive(Primitive::Number),
        ]);
        let other = Type::union(vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::String),
        ]);
        assert_eq!(one, other);
    }

    #[test]
    fn primitives_sort_before_nominals() {
        let Some(Type::Union(members)) = Type::union(vec![
            Type::Nominal {
                name: "Decimal".to_owned(),
                symbol: None,
            },
            Type::Primitive(Primitive::Number),
        ]) else {
            panic!("two distinct members make a union");
        };
        assert_eq!(members[0], Type::Primitive(Primitive::Number));
    }

    #[test]
    fn a_repeated_member_appears_once() {
        assert_eq!(
            Type::union(vec![
                Type::Primitive(Primitive::Number),
                Type::Primitive(Primitive::Number),
            ]),
            Some(Type::Primitive(Primitive::Number))
        );
    }

    /// Flattening is one level, matching what the authoring surface documents.
    #[test]
    fn a_nested_union_flattens_one_level() {
        let inner = Type::union(vec![
            Type::Primitive(Primitive::Number),
            Type::Primitive(Primitive::String),
        ])
        .expect("two members");
        let Some(Type::Union(members)) =
            Type::union(vec![inner, Type::Primitive(Primitive::Boolean)])
        else {
            panic!("three distinct members make a union");
        };
        assert_eq!(members.len(), 3);
    }

    /// The identity digest is populated and constant within a process.
    ///
    /// The property that matters — it changes when the oracle's source changes — is
    /// structural rather than testable: `build.rs` hashes all of `src/` under
    /// `rerun-if-changed`, and a test able to fail would have to edit its own source. This
    /// asserts what can be asserted: the build script ran and produced something.
    #[test]
    fn the_oracle_identity_is_populated_and_stable() {
        let once = crate::oracle_identity();
        assert_ne!(once, [0_u8; 32], "the build script did not write a digest");
        assert_eq!(once, crate::oracle_identity());
    }
}
