//! The committed `packages/lanekeep/index.d.ts` is the file this crate renders.
//!
//! `world.wit` is the source of truth and `index.d.ts` is its rendering, so the two can only
//! disagree if someone edited the rendering by hand — which is exactly what a generated file
//! must not be. This test regenerates and diffs, the way `fixture_currency.rs` regenerates and
//! diffs the committed fixtures against their sources. `LANEKEEP_BLESS_INDEX_DTS=1` rewrites the
//! committed file instead of failing, for the case where the world changed and the rendering is
//! being re-recorded.

use lanekeep_types_gen::render_index_dts;

#[test]
fn the_committed_index_dts_is_the_generated_one() {
    let wit = include_str!("../../lanekeep-wasm/wit/world.wit");
    let generated = render_index_dts(wit);
    let committed = include_str!("../../../packages/lanekeep/index.d.ts");

    if std::env::var("LANEKEEP_BLESS_INDEX_DTS").is_ok() {
        std::fs::write("../../packages/lanekeep/index.d.ts", &generated).expect("writes");
    }

    assert_eq!(
        generated, committed,
        "`packages/lanekeep/index.d.ts` has drifted from `world.wit` — run `just generate-index-dts`"
    );
}
