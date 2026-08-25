//! Writes `packages/lanekeep/package.json` and `packages/lanekeep/types.test-d.ts` from
//! `COMPONENT_RULES`.
//!
//! Run from the repository root via `just generate-builtin-subpaths`. The paths are relative
//! to the root on purpose: the recipe is the single way to regenerate the built-in subpath
//! mapping, and a path spelled here is one fewer to drift from the recipe.

// A command-line tool: reporting a read/write failure on stderr is its whole job.
#![allow(
    clippy::print_stderr,
    reason = "a CLI reporting a file failure on stderr is the behavior, not a lint to silence"
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let pkg_path = "packages/lanekeep/package.json";
    let test_path = "packages/lanekeep/types.test-d.ts";

    let current = match std::fs::read_to_string(pkg_path) {
        Ok(s) => s,
        Err(error) => {
            eprintln!("cannot read `{pkg_path}`: {error}");
            return ExitCode::FAILURE;
        }
    };

    let package = lanekeep_package_gen::render_package_json(&current);
    if let Err(error) = std::fs::write(pkg_path, package) {
        eprintln!("cannot write `{pkg_path}`: {error}");
        return ExitCode::FAILURE;
    }

    let test_dts = lanekeep_package_gen::render_types_test_dts();
    if let Err(error) = std::fs::write(test_path, test_dts) {
        eprintln!("cannot write `{test_path}`: {error}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
