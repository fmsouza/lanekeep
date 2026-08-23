//! Writes `packages/lanekeep/index.d.ts` from `crates/lanekeep-wasm/wit/world.wit`.
//!
//! Run from the repository root via `just generate-index-dts`. The paths are relative to the
//! root on purpose: the recipe is the single way to regenerate the published definitions, and a
//! path spelled here is one fewer to drift from the recipe.

// A command-line tool: reporting a read/write failure on stderr is its whole job.
#![allow(
    clippy::print_stderr,
    reason = "a CLI reporting a file failure on stderr is the behavior, not a lint to silence"
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let wit = match std::fs::read_to_string("crates/lanekeep-wasm/wit/world.wit") {
        Ok(wit) => wit,
        Err(error) => {
            eprintln!("cannot read `crates/lanekeep-wasm/wit/world.wit`: {error}");
            return ExitCode::FAILURE;
        }
    };

    let rendered = lanekeep_types_gen::render_index_dts(&wit);

    if let Err(error) = std::fs::write("packages/lanekeep/index.d.ts", rendered) {
        eprintln!("cannot write `packages/lanekeep/index.d.ts`: {error}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
