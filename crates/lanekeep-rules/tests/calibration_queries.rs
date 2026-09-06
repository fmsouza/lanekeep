//! Proves the committed calibration rule's transcribed queries (scripts/calibration/
//! no-secret-in-string.calib.ts) fire on each source shape the pera-react-native corpus
//! exhibits, and honor the #218 sanitizer. This guards the transcription: a query mistyped
//! here silently changes what the #220 calibration measures. `include_str!` couples the test
//! to the committed file, so moving or renaming it breaks compilation rather than drifting.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helper below is neither, so the grant it already \
              makes for unit tests has to be restated for it."
)]

use lanekeep_testkit::RuleTester;

const CALIB_RULE: &str = include_str!("../../../scripts/calibration/no-secret-in-string.calib.ts");

fn tester() -> RuleTester {
    RuleTester::new("calibration", CALIB_RULE).expect("builds the calibration project")
}

#[test]
fn withsecret_bare_arrow_parameter_flows_to_a_sink() {
    // #217: a source captured on a callback parameter binding is now seeded.
    tester()
        .reports_at("withSecret(sk => console.log(sk))\n", &[(1, 30)])
        .expect("param-origin taint reaches the sink");
}

#[test]
fn withsecret_parenthesized_arrow_parameter_flows_to_a_sink() {
    tester()
        .reports_at("withSecret((sk) => console.log(sk))\n", &[(1, 32)])
        .expect("param-origin taint reaches the sink (parenthesized form)");
}

#[test]
fn a_direct_return_accessor_flows_to_a_sink() {
    tester()
        .reports_at(
            "const m = entropyToMnemonic(x); console.log(m)\n",
            &[(1, 45)],
        )
        .expect("direct-return source reaches the sink");
}

#[test]
fn a_property_read_flows_to_a_sink() {
    tester()
        .reports_at("const k = obj.secretKey; console.log(k)\n", &[(1, 38)])
        .expect("property-read source reaches the sink");
}

#[test]
fn field_insensitivity_still_reports_dot_length() {
    // The documented over-approximation: `.length` of a secret is not the secret, but a
    // field-insensitive analysis taints every property read. This asserts the FP the doc
    // attributes to field-insensitivity is still present (it is the candidate B4).
    tester()
        .reports_at(
            "const k = obj.secretKey; console.log(`${k.length}`)\n",
            &[(1, 41)],
        )
        .expect("field-insensitive taint still reports .length");
}

#[test]
fn a_sanitizer_wrapping_the_source_in_a_compound_sink_is_honored() {
    // #218: a source wrapped by a sanitizer inside a compound sink expression must not report.
    tester()
        .accepts("console.log(`x${describeBytes(obj.secretKey)}`)\n")
        .expect("the compound-sink sanitizer suppresses the flow");
}

#[test]
fn a_non_secret_argument_is_silent() {
    tester()
        .accepts("console.log(harmless)\n")
        .expect("no source, no report");
}
