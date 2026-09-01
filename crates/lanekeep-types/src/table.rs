//! What an operator or a builtin conversion produces.
//!
//! Closed and small on purpose. Every entry is something TypeScript guarantees about a
//! result regardless of what the operands turn out to be at run time, so an entry that
//! needed a caveat does not belong here — it belongs in the `None` that everything outside
//! this file gets.

use crate::Primitive;

/// What a binary operator produces, given whatever is known about its operands.
///
/// `left` and `right` are `None` when the oracle could not type that side. Only the
/// comparisons ignore them — a comparison is boolean whatever it compares. Every
/// arithmetic operator reads both, because TypeScript's arithmetic is not one type: the
/// same `*` is `number` between numbers and `bigint` between bigints, and is a type error
/// across the two.
pub(crate) fn binary(
    operator: &str,
    left: Option<Primitive>,
    right: Option<Primitive>,
) -> Option<Primitive> {
    match operator {
        // A comparison is boolean whatever it compares, including operands the oracle could
        // not type at all. This is the one place unknown operands still give an answer.
        "<" | ">" | "<=" | ">=" | "==" | "!=" | "===" | "!==" => Some(Primitive::Boolean),

        // Arithmetic other than `+` is `plus` without the string case: `number` between
        // numbers, `bigint` between bigints, and nothing at all otherwise.
        //
        // The `_ => Some(Number)` this replaces was the same guess `plus` refuses one arm
        // below, made where nothing had been established. It answered `number` for
        // `total * 2n` with `total` imported, and for `new D() * new D()` — the first a
        // bigint and the second a `TypeError`, and both reported to a rule as a plain
        // number with nothing to say the operands were never typed.
        "-" | "*" | "/" | "%" | "**" => match (left?, right?) {
            (Primitive::Number, Primitive::Number) => Some(Primitive::Number),
            (Primitive::BigInt, Primitive::BigInt) => Some(Primitive::BigInt),
            _ => None,
        },

        "+" => plus(left, right),

        // Everything else — `??`, `&&`, `||`, the bitwise operators, `in`, `instanceof` —
        // is deliberately outside the table. Their results depend on values rather than on
        // types, and guessing would be worse than saying nothing.
        _ => None,
    }
}

/// `+`, the one operator whose result depends on its operands.
///
/// String concatenation wins over arithmetic, `bigint` only combines with `bigint`, and
/// anything else is refused rather than guessed at — `1 + 1n` is a `TypeError` at run time,
/// and an oracle answering `number` for it would have a rule report about a value that
/// never exists.
fn plus(left: Option<Primitive>, right: Option<Primitive>) -> Option<Primitive> {
    match (left?, right?) {
        (Primitive::String, _) | (_, Primitive::String) => Some(Primitive::String),
        (Primitive::Number, Primitive::Number) => Some(Primitive::Number),
        (Primitive::BigInt, Primitive::BigInt) => Some(Primitive::BigInt),
        _ => None,
    }
}

/// What a unary operator produces.
///
/// Only `typeof`, whose result is a string no matter what it is applied to. `!` is boolean
/// in practice and is left out because nothing needs it yet; `-` on a `bigint` is a
/// `bigint` and on anything else a `number`, which is the `+` problem in miniature and is
/// not worth the arm until a rule wants it.
pub(crate) fn unary(operator: &str) -> Option<Primitive> {
    match operator {
        "typeof" => Some(Primitive::String),
        _ => None,
    }
}

/// What one of the global conversion functions produces.
///
/// The caller is responsible for having established that the name resolves to no local
/// binding. A file declaring its own `parseFloat` must not be typed by this table, and this
/// function cannot tell — it is handed a name.
pub(crate) fn builtin_call(callee: &str) -> Option<Primitive> {
    match callee {
        "parseFloat" | "parseInt" | "Number" => Some(Primitive::Number),
        "String" => Some(Primitive::String),
        "BigInt" => Some(Primitive::BigInt),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Primitive;

    #[test]
    fn comparisons_are_boolean_whatever_their_operands() {
        for operator in ["<", ">", "<=", ">=", "==", "!=", "===", "!=="] {
            assert_eq!(
                binary(operator, None, None),
                Some(Primitive::Boolean),
                "{operator}"
            );
        }
    }

    #[test]
    fn the_arithmetic_operators_other_than_plus_are_number_between_numbers() {
        for operator in ["-", "*", "/", "%", "**"] {
            assert_eq!(
                binary(operator, Some(Primitive::Number), Some(Primitive::Number)),
                Some(Primitive::Number),
                "{operator}"
            );
        }
    }

    #[test]
    fn the_arithmetic_operators_other_than_plus_are_bigint_between_bigints() {
        for operator in ["-", "*", "/", "%", "**"] {
            assert_eq!(
                binary(operator, Some(Primitive::BigInt), Some(Primitive::BigInt)),
                Some(Primitive::BigInt),
                "{operator}"
            );
        }
    }

    /// The half that denies the bug: an operand nothing established is not a number.
    ///
    /// This arm used to fall through to `Some(Number)` whatever the operands were, which
    /// answered `number` for an expression whose sides had never been typed at all. A rule
    /// reading that reports about a value it has no evidence for, which is the one failure
    /// this oracle is arranged against.
    #[test]
    fn arithmetic_with_an_unknown_operand_is_unknown() {
        for operator in ["-", "*", "/", "%", "**"] {
            assert_eq!(binary(operator, None, None), None, "{operator}");
            assert_eq!(
                binary(operator, None, Some(Primitive::Number)),
                None,
                "{operator}"
            );
            assert_eq!(
                binary(operator, Some(Primitive::Number), None),
                None,
                "{operator}"
            );
        }
    }

    /// The same refusal `plus` makes, one operator family over.
    ///
    /// `1 * 1n` is a `TypeError` at run time, exactly as `1 + 1n` is. Mixing anything else
    /// in — a string, a boolean — is a type error TypeScript rejects, so there is no result
    /// to name.
    #[test]
    fn arithmetic_refuses_a_mixed_pair() {
        for operator in ["-", "*", "/", "%", "**"] {
            for (left, right) in [
                (Primitive::Number, Primitive::BigInt),
                (Primitive::BigInt, Primitive::Number),
                (Primitive::String, Primitive::Number),
                (Primitive::Boolean, Primitive::Boolean),
            ] {
                assert_eq!(
                    binary(operator, Some(left), Some(right)),
                    None,
                    "{operator}"
                );
            }
        }
    }

    #[test]
    fn plus_is_string_when_either_side_is() {
        assert_eq!(
            binary("+", Some(Primitive::String), Some(Primitive::Number)),
            Some(Primitive::String)
        );
        assert_eq!(
            binary("+", Some(Primitive::Number), Some(Primitive::String)),
            Some(Primitive::String)
        );
    }

    #[test]
    fn plus_is_number_when_both_sides_are() {
        assert_eq!(
            binary("+", Some(Primitive::Number), Some(Primitive::Number)),
            Some(Primitive::Number)
        );
    }

    #[test]
    fn plus_is_bigint_when_both_sides_are() {
        assert_eq!(
            binary("+", Some(Primitive::BigInt), Some(Primitive::BigInt)),
            Some(Primitive::BigInt)
        );
    }

    /// The arm that must not guess.
    ///
    /// `1 + 1n` is a `TypeError` at run time. An oracle answering `number` here would have
    /// a rule report about a value that never exists.
    #[test]
    fn plus_refuses_to_mix_number_and_bigint() {
        assert_eq!(
            binary("+", Some(Primitive::Number), Some(Primitive::BigInt)),
            None
        );
        assert_eq!(
            binary("+", Some(Primitive::BigInt), Some(Primitive::Number)),
            None
        );
    }

    #[test]
    fn plus_with_an_unknown_operand_is_unknown() {
        assert_eq!(binary("+", None, Some(Primitive::Number)), None);
        assert_eq!(binary("+", Some(Primitive::Number), None), None);
    }

    /// Deliberately absent from the table, and asserted so rather than left to chance.
    #[test]
    fn an_operator_outside_the_table_is_unknown() {
        for operator in [
            "??",
            "&&",
            "||",
            "&",
            "|",
            "^",
            "<<",
            ">>",
            "in",
            "instanceof",
        ] {
            assert_eq!(
                binary(operator, Some(Primitive::Number), Some(Primitive::Number)),
                None,
                "{operator}"
            );
        }
    }

    #[test]
    fn typeof_is_string_and_the_other_unary_operators_are_not_in_the_table() {
        assert_eq!(unary("typeof"), Some(Primitive::String));
        assert_eq!(unary("!"), None);
        assert_eq!(unary("-"), None);
        assert_eq!(unary("void"), None);
    }

    #[test]
    fn the_builtin_conversions_are_their_own_types() {
        assert_eq!(builtin_call("parseFloat"), Some(Primitive::Number));
        assert_eq!(builtin_call("parseInt"), Some(Primitive::Number));
        assert_eq!(builtin_call("Number"), Some(Primitive::Number));
        assert_eq!(builtin_call("String"), Some(Primitive::String));
        assert_eq!(builtin_call("BigInt"), Some(Primitive::BigInt));
    }

    #[test]
    fn a_function_outside_the_table_is_unknown() {
        assert_eq!(builtin_call("Boolean"), None);
        assert_eq!(builtin_call("myHelper"), None);
    }
}
