//! The committed `packages/lanekeep/package.json` and `types.test-d.ts` are the files this
//! crate renders.
//!
//! `COMPONENT_RULES` is the source of truth and these files are its rendering, so the two can
//! only disagree if someone edited the rendering by hand — which is exactly what a generated
//! file must not be. This test regenerates and diffs, the way
//! `crates/lanekeep-types-gen/tests/generated.rs` regenerates and diffs the committed
//! `index.d.ts` against `world.wit`. `LANEKEEP_BLESS_PACKAGE_JSON=1` rewrites the committed
//! files instead of failing, for the case where `COMPONENT_RULES` changed and the rendering is
//! being re-recorded.

use lanekeep_package_gen::{render_package_json, render_types_test_dts};

#[test]
fn the_committed_package_json_is_the_generated_one() {
    let committed = include_str!("../../../packages/lanekeep/package.json");
    let generated = render_package_json(committed);

    if std::env::var("LANEKEEP_BLESS_PACKAGE_JSON").is_ok() {
        std::fs::write("../../../packages/lanekeep/package.json", &generated).expect("writes");
    }

    // A Windows checkout with `core.autocrlf` on holds the committed file under CRLF bytes,
    // while the renderer emits LF. Fold CRLF to LF before comparing, on the same terms as
    // `crates/lanekeep-types-gen/tests/generated.rs`'s `fold`.
    assert_eq!(
        generated,
        fold_crlf(committed),
        "`packages/lanekeep/package.json` has drifted from `COMPONENT_RULES` — run `just generate-builtin-subpaths`"
    );
}

#[test]
fn the_committed_types_test_dts_is_the_generated_one() {
    let committed = include_str!("../../../packages/lanekeep/types.test-d.ts");
    let generated = render_types_test_dts();

    if std::env::var("LANEKEEP_BLESS_PACKAGE_JSON").is_ok() {
        std::fs::write("../../../packages/lanekeep/types.test-d.ts", &generated).expect("writes");
    }

    assert_eq!(
        generated,
        fold_crlf(committed),
        "`packages/lanekeep/types.test-d.ts` has drifted from `COMPONENT_RULES` — run `just generate-builtin-subpaths`"
    );
}

/// CRLF to LF, leaving a lone carriage return alone.
fn fold_crlf(text: &str) -> String {
    text.replace("\r\n", "\n")
}
