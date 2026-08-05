//! What the host makes of a fact before it becomes one.
//!
//! # The check QuickJS never had to make
//!
//! Under QuickJS a fact is a JavaScript value and `JSON.stringify` *is* the definition of
//! serializable rather than a test applied alongside it: what a rule can emit is exactly what
//! survives that call, so the payload reaching the host is well-formed JSON and an object by
//! construction. `lanekeep-js` therefore checks `kind` and nothing else, because after
//! stringify there is nothing else left to check.
//!
//! A component hands the host a `string`. Nothing upstream says it is JSON at all — it was
//! produced by whatever serializer the guest's language happens to have, on the far side of a
//! boundary that carries bytes. So the shape that was structurally free becomes work this
//! module owns, and `emit-fact` is the one place in the host API where the *contract* changes
//! rather than the calling convention.
//!
//! The two ways to get it wrong are not symmetric. Permissive, and a malformed payload is
//! written into a cache entry and surfaces in the reduce phase — as far from the rule that
//! emitted it as the design allows, on a later run, possibly in another rule's output. Strict,
//! and a legitimate rule cannot emit at all, which at least says so immediately.
//!
//! # Three failures, and every one of them is a value
//!
//! `wit/world.wit` declares `emit-fact` as `result<_, fact-error>` with exactly these three
//! cases, so a refusal is something the rule receives and can act on rather than a trap that
//! ends the invocation. That was decided in the world and this module implements it, but it is
//! worth knowing why it is right, because the immediately preceding decision on this surface
//! went the other way: a read on a host with no [`lanekeep_core::files::FileAccess`] *fails the
//! call*, deliberately, and `crate::host`'s module header sets out the reasoning.
//!
//! The difference is where the fault lives. "This host granted no file access" is a fact about
//! the *host*, which is not one of `(bytes, path, ruleset, config, tracked reads)` — letting a
//! rule handle it would let a run's output depend on something the cache key cannot see. A
//! malformed payload is a fact about the *rule's own input*: the same rule over the same file
//! with the same config produces the same string and gets the same answer, so a rule that
//! catches it and reports something has changed nothing the cache does not already know.
//!
//! It is also not the divergence from QuickJS it first looks like. `lanekeep-js` throws, which
//! ends the invocation *unless the rule catches it* — and a rule may — so on both engines a
//! bad fact is rejected, is not recorded, and is recoverable by a rule that wants to recover.
//!
//! # Checked, and deliberately not checked
//!
//! `kind` must be non-empty, which is `lanekeep-js`'s rule restated: `kind` is what
//! `facts(kind)` selects on, so a fact without one can never be read back, and accepting it
//! would leave a rule looking correct right up until the reduce phase found nothing. Nothing
//! else about `kind` is policed — a kind of `" "` is accepted here exactly as it is under
//! QuickJS, because inventing a stricter rule for one engine would make the same rule behave
//! differently depending on which one ran it.
//!
//! `data` must parse as JSON, and must parse to an object. Beyond that its contents are the
//! rule's business: a fact is whatever shape its rule chose, and policing that shape would make
//! it part of lanekeep's public API rather than the rule's private business — which is the
//! reasoning [`lanekeep_core::Fact`] already carries for storing the serialized form at all.
//!
//! `kind` is checked first, matching the order `lanekeep-js` checks in. Two things can be wrong
//! at once and a `variant` reports one of them; the world offers no case meaning "several", and
//! choosing which to report by which check is cheaper would make the answer an artifact of the
//! implementation rather than of the input.
//!
//! # The bytes are parsed and then thrown away
//!
//! Nothing is re-serialized. What the host records is the guest's own string, byte for byte,
//! and the parsed [`serde_json::Value`] exists only long enough to answer "is this an object".
//!
//! That is a determinism decision and not an efficiency one. Re-serializing would put
//! `serde_json`'s own map ordering into the payload that reaches the cache, and that ordering is
//! a build-time property of this workspace — `serde_json`'s `preserve_order` feature swaps a
//! `BTreeMap` for an `IndexMap`, so the same fact would hash differently depending on a feature
//! flag no rule author can see. The guest's bytes are already deterministic for the guest that
//! produced them, which is the only guarantee the cache needs.
//!
//! # The cycle case has no analog here, and that is structural
//!
//! `lanekeep-js` has a fourth failure: a fact holding a reference to itself, which
//! `JSON.stringify` refuses rather than hanging. There is nothing to test for here, and no test
//! is written, because a cycle cannot survive being rendered to a JSON string — and that
//! rendering happened entirely on the guest side, in the guest's own language, before the host
//! saw a `string` at all. A guest whose serializer hangs on a cycle hangs inside its own
//! instance, where the run budget is what stops it.

use serde_json::Value;

use crate::bindings::types::FactError;

/// Whether a component's fact may be recorded, and why not when it may not.
///
/// Split out from the host method around it so the three answers can be exercised without a
/// wasmtime store, a component, or a parsed file — none of which have anything to do with the
/// question. `tests/facts.rs` drives the same three through a real guest, which is what shows
/// the cases survive the boundary; this is where the cases themselves are pinned down.
///
/// # Errors
///
/// [`FactError::EmptyKind`] when `kind` is empty, [`FactError::InvalidJson`] carrying the
/// parser's own message when `data` is not JSON, and [`FactError::NotAnObject`] when it is JSON
/// but not an object.
pub(crate) fn validate(kind: &str, data: &str) -> Result<(), FactError> {
    if kind.is_empty() {
        return Err(FactError::EmptyKind);
    }

    match serde_json::from_str::<Value>(data) {
        Ok(Value::Object(_)) => Ok(()),
        Ok(_) => Err(FactError::NotAnObject),
        // The parser's message rather than one written here. It names a position in the
        // payload, which is the only thing anyone debugging a guest's serializer can use, and
        // it is a pure function of the input — so it is safe to put in front of a rule whose
        // result is cached.
        Err(problem) => Err(FactError::InvalidJson(problem.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which case came back, flattened so a test can compare against a literal.
    ///
    /// `FactError` carries no `PartialEq` — wasmtime's `bindgen!` does not derive one — and a
    /// test matching on the variant while ignoring the payload would pass a host that returned
    /// the right case with the wrong message.
    fn outcome(result: Result<(), FactError>) -> String {
        match result {
            Ok(()) => "ok".to_owned(),
            Err(FactError::EmptyKind) => "empty-kind".to_owned(),
            Err(FactError::NotAnObject) => "not-an-object".to_owned(),
            Err(FactError::InvalidJson(message)) => format!("invalid-json({message})"),
        }
    }

    #[test]
    fn an_object_is_accepted() {
        assert_eq!(outcome(validate("export", r#"{"symbol":"parse"}"#)), "ok");
    }

    #[test]
    fn an_empty_object_is_accepted() {
        // A rule that observed something about a file and needs nothing but the `kind` to say
        // so is emitting a real fact. `{}` is the payload it has.
        assert_eq!(outcome(validate("export", "{}")), "ok");
    }

    #[test]
    fn surrounding_whitespace_is_accepted() {
        // A serializer that pretty-prints is not a serializer that is wrong, and the guest's
        // language chose it. What matters is that the bytes parse to an object.
        assert_eq!(outcome(validate("export", "  {\n  \"n\": 1\n}\n")), "ok");
    }

    #[test]
    fn an_empty_kind_is_rejected() {
        // Ahead of the payload, and the payload here is valid: a fact with no kind can never
        // be selected by `facts(kind)`, so it is refused whatever it carries.
        assert_eq!(outcome(validate("", r#"{"symbol":"parse"}"#)), "empty-kind");
    }

    #[test]
    fn an_empty_kind_is_reported_ahead_of_a_malformed_payload() {
        // Both are wrong and the world can report one. Which one is a property of the input
        // rather than of which check happens to run first, so it is asserted rather than left
        // to the reading order of the function.
        assert_eq!(outcome(validate("", "{oops")), "empty-kind");
    }

    #[test]
    fn a_kind_that_is_only_whitespace_is_accepted() {
        // Not because it is a good kind, but because `lanekeep-js` accepts it and two engines
        // that disagree about what a rule may emit is worse than either rule alone.
        assert_eq!(outcome(validate(" ", "{}")), "ok");
    }

    #[test]
    fn text_that_is_not_json_is_rejected_with_the_parser_message() {
        // The message is compared against what `serde_json` itself says for the same input,
        // not against a literal written here: a host that invented its own wording would pass
        // a substring check and fail this.
        let data = "{\"symbol\":\"parse\"";
        let reported = serde_json::from_str::<Value>(data)
            .expect_err("an unterminated object does not parse")
            .to_string();

        assert_eq!(
            outcome(validate("export", data)),
            format!("invalid-json({reported})")
        );
        assert!(
            !reported.is_empty(),
            "the case carries a message rather than an empty string"
        );
    }

    #[test]
    fn an_empty_payload_is_not_json() {
        // A guest that sent nothing at all. It is `invalid-json` rather than `not-an-object`,
        // because there is no value there to have the wrong type.
        assert!(outcome(validate("export", "")).starts_with("invalid-json("));
    }

    #[test]
    fn json_that_is_not_an_object_is_rejected() {
        // Every other thing a JSON document can be. `null` is the one worth naming: it parses,
        // it is a perfectly good JSON document, and a fact made of it carries nothing the
        // reduce phase could read.
        for data in ["[1,2,3]", "\"export\"", "42", "null", "true"] {
            assert_eq!(
                outcome(validate("export", data)),
                "not-an-object",
                "`{data}` is JSON and is not an object"
            );
        }
    }

    #[test]
    fn trailing_content_after_an_object_is_rejected() {
        // `{}` followed by anything is not one JSON document. A check that looked only at the
        // first character would accept this and hand the reduce phase a payload no parser will
        // read back.
        assert!(outcome(validate("export", "{} junk")).starts_with("invalid-json("));
        assert!(outcome(validate("export", "{}{}")).starts_with("invalid-json("));
    }

    #[test]
    fn a_payload_nested_far_deeper_than_anything_real_is_refused_rather_than_fatal() {
        // The property is not the depth limit, it is that a hostile payload is *answered*. The
        // host is handed bytes by a component it did not write, and this workspace denies
        // `panic!` outside tests because an engine that dies on a malformed input has failed at
        // its job — a stack overflow is worse still, since it takes the process with it and
        // nothing catches it.
        //
        // `serde_json` bounds recursion for exactly this reason. Asserted rather than assumed,
        // because the bound is a property of that crate and could change under a bump.
        let deep = format!("{}{}", "[".repeat(2000), "]".repeat(2000));
        assert!(outcome(validate("export", &deep)).starts_with("invalid-json("));

        // And the same shape as an object, which is the one that would otherwise reach the
        // object branch rather than the error branch.
        let deep_object = format!("{}1{}", r#"{"a":"#.repeat(2000), "}".repeat(2000));
        assert!(outcome(validate("export", &deep_object)).starts_with("invalid-json("));
    }
}
