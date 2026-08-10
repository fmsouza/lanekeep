//! A guest that fails *gracefully*, and how that differs from one that traps.
//!
//! Measured 2026-08-10 and recorded in
//! `docs/superpowers/specs/2026-08-10-componentize-js-spike-findings.md`: an uncaught JavaScript
//! throw inside a component reaches the host as `wasm trap: wasm 'unreachable' instruction
//! executed`, with the message, the error type and the stack all gone, because `check` used to
//! return `()` and there was no channel. Caught, the stack is intact and carries real positions.
//! So the world's `rule-error` is what a later source-map remapping is built on, and there is
//! nothing to remap without it.
#![expect(
    clippy::expect_used,
    reason = "helpers in a tests/ crate are outside clippy.toml's allow-expect-in-tests"
)]

mod common;

use lanekeep_wasm::WasmError;

#[test]
fn a_guest_that_fails_reports_its_message_and_frames() {
    let (mut runtime, slots) = common::runtime_for_all("two-rules");
    // Rule 1 of the fixture fails deliberately when handed the `boom` capture.
    let outcome = runtime.check_capture(slots[1], "boom");

    let error = outcome.expect_err("the guest reports a failure");
    assert!(
        matches!(error, WasmError::RuleFailed { .. }),
        "a returned failure is not a trap: {error:?}"
    );
    let WasmError::RuleFailed {
        message, frames, ..
    } = error
    else {
        unreachable!("matched above")
    };
    assert_eq!(message, "deliberate failure from a rule");
    assert!(
        frames.iter().any(|f| f.function == "boom"),
        "the innermost frame survives the boundary: {frames:?}"
    );
}
