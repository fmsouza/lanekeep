//! Rebuilds the four shipped WebAssembly components from source when their committed
//! artifacts are missing.
//!
//! The four artifacts (`go-builtins.wasm`, `no-glob-import.wasm`, `no-unwrap.wasm`,
//! `typescript-builtins.wasm` and its `.map`) are committed and read with `include_bytes!`,
//! so a normal build — and, critically, `cargo publish`'s verify build, where the sources
//! these were built from are absent from the package — must be a strict no-op. Only when an
//! artifact is actually missing do we reach for a toolchain, and only then do we emit
//! `rerun-if-changed` directives, so a checkout that has everything never re-runs the
//! script and a package that has only the four artifacts never attempts a rebuild.
//!
//! This deliberately does *not* shell out to `just`: the underlying commands are invoked
//! directly, so the build is self-contained and reproduces the `just rust-rules`,
//! `just go-rules` and `just typescript-builtins` recipes bit-for-bit.

// A build script is not the gated library: it is a packaging shim whose whole job is to
// shell out, print directives to cargo and `panic!` on a missing tool, and the workspace's
// `[lints]` (escalated to `-D warnings` by `just lint`) forbid each of those. They are
// relaxed here, not silenced: every panic still names the missing tool and its `just`
// recipe equivalent, which is the actionable behavior requirement 6 asks for.
#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::manual_assert,
    clippy::unnecessary_debug_formatting,
    missing_docs,
    missing_debug_implementations
)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // The manifest is `crates/lanekeep-rules/Cargo.toml`, so the repo root is two parents up.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("`{manifest_dir:?}` is not two levels under the repo root"))
        .to_path_buf();
    let components_dir = manifest_dir.join("components");

    // The directory is gitignored and empty on a fresh checkout, so the build helpers below need
    // it to exist before they can write an artifact into it. Idempotent, and a no-op when the
    // artifacts are already present.
    fs::create_dir_all(&components_dir)
        .unwrap_or_else(|e| panic!("failed to create `{}`: {e}", components_dir.display()));

    // Declare every input this script reads *up front and unconditionally*. Two reasons:
    //
    // 1. Cargo re-runs the build script when any `rerun-if-changed` path is created, deleted or
    //    modified — and the whole point here is that a fresh clone *has no* `components/*.wasm`.
    //    If the output paths are only declared after they are observed absent, an incremental
    //    build that loses a `.wasm` out from under cargo (exactly what `git checkout` of this
    //    change does) never re-runs the script and `include_bytes!` fails with "file not found".
    //    Declaring the outputs as inputs makes their *absence* a change cargo sees.
    //
    // 2. Emitting nothing would make cargo re-run the script on every build — a wasteful no-op
    //    with the toolchains installed, and a spurious failure without them. Declaring the real
    //    sources and outputs means a build whose inputs are all satisfied compiles without ever
    //    touching a toolchain.
    let world_wit = repo_root.join("crates/lanekeep-wasm/wit/world.wit");
    for input in [
        repo_root.join("go-rules"),
        world_wit.clone(),
        repo_root.join("rust-rules/no-glob-import/src/lib.rs"),
        repo_root.join("rust-rules/no-unwrap/src/lib.rs"),
        repo_root.join("crates/lanekeep-rules/typescript"),
        repo_root.join("packages/lanekeep/runtime"),
        // The artifacts themselves: their absence must re-run this script.
        components_dir.join("go-builtins.wasm"),
        components_dir.join("no-glob-import.wasm"),
        components_dir.join("no-unwrap.wasm"),
        components_dir.join("typescript-builtins.wasm"),
        components_dir.join("typescript-builtins.wasm.map"),
    ] {
        stdout(&format!("cargo:rerun-if-changed={}", input.display()));
    }

    if !components_dir.join("go-builtins.wasm").exists() {
        build_go_builtins(&repo_root, &components_dir);
    }

    if !components_dir.join("no-glob-import.wasm").exists() {
        build_rust_component(&repo_root, &components_dir, "no-glob-import");
    }

    if !components_dir.join("no-unwrap.wasm").exists() {
        build_rust_component(&repo_root, &components_dir, "no-unwrap");
    }

    // The TypeScript component ships with its source map sidecar, and `lib.rs` embeds both — so
    // either missing is a missing artifact, and the check must match what `include_bytes!` reads.
    if !components_dir.join("typescript-builtins.wasm").exists()
        || !components_dir.join("typescript-builtins.wasm.map").exists()
    {
        build_typescript_builtins(&repo_root, &components_dir);
    }
}

/// Rebuild `go-builtins.wasm` with TinyGo, reproducing `just go-rules`.
fn build_go_builtins(repo_root: &Path, components_dir: &Path) {
    // `tinygo build` also invokes `wasm-opt` (WASMOPT) and `wasm-tools` internally, but the
    // recipe's own guard is `tinygo`; the failure of those two surfaces from inside the build
    // with the toolchain naming them.
    require_tool("tinygo", "just go-rules");

    let status = Command::new("tinygo")
        .arg("build")
        .arg("-target=wasm-unknown")
        .arg("-wit-package")
        .arg(repo_root.join("crates/lanekeep-wasm/wit"))
        .arg("-wit-world")
        .arg("rule")
        .arg("-panic=trap")
        .arg("-no-debug")
        .arg("-o")
        .arg(components_dir.join("go-builtins.wasm"))
        .arg(".")
        .current_dir(repo_root.join("go-rules"))
        .status()
        .unwrap_or_else(|e| {
            panic!("failed to run `tinygo build` to build `go-builtins.wasm`: {e}")
        });

    if !status.success() {
        panic!(
            "`tinygo build` failed to build `go-builtins.wasm` (the `just go-rules` equivalent)"
        );
    }
}

/// Rebuild one of the two shipped Rust components with `cargo component`, reproducing
/// `just rust-rules` for that one crate. `name` is the directory name with hyphens, e.g.
/// `no-unwrap`; cargo names the artifact with hyphens turned into underscores.
fn build_rust_component(repo_root: &Path, components_dir: &Path, name: &str) {
    require_tool("cargo-component", "just rust-rules");

    let underscore_name = name.replace('-', "_");

    let status = Command::new("cargo")
        .arg("component")
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .current_dir(repo_root.join("rust-rules").join(name))
        .status()
        .unwrap_or_else(|e| {
            panic!("failed to run `cargo component build` to build `{name}.wasm`: {e}")
        });

    if !status.success() {
        panic!(
            "`cargo component build` failed to build `{name}.wasm` (the `just rust-rules` equivalent)"
        );
    }

    fs::copy(
        repo_root
            .join("rust-rules/target/wasm32-unknown-unknown/release")
            .join(format!("{underscore_name}.wasm")),
        components_dir.join(format!("{name}.wasm")),
    )
    .unwrap_or_else(|e| panic!("failed to copy the built `{name}.wasm` into components/: {e}"));
}

/// Rebuild `typescript-builtins.wasm` (and its `.map`) with `jco componentize`,
/// reproducing `just typescript-builtins` (which routes through `_componentize`).
fn build_typescript_builtins(repo_root: &Path, components_dir: &Path) {
    require_tool("node", "just typescript-builtins");
    require_tool("npx", "just typescript-builtins");

    let jco = repo_root.join("packages/lanekeep/node_modules/.bin/jco");
    if !jco.is_file() {
        panic!(
            "error: 'packages/lanekeep' has no installed jco.\n\
             run `npm --prefix packages/lanekeep ci` first, or run `just typescript-builtins`."
        );
    }

    let status = Command::new("npx")
        .arg("--no-install")
        .arg("--prefix")
        .arg(repo_root.join("packages/lanekeep"))
        .arg("jco")
        .arg("componentize")
        .arg(repo_root.join("crates/lanekeep-rules/typescript/entry.ts"))
        .arg("--wit")
        .arg(repo_root.join("crates/lanekeep-wasm/wit"))
        .arg("--world-name")
        .arg("rule")
        .arg("--disable")
        .arg("all")
        .arg("--bundle")
        .arg("--bundle-config")
        .arg(repo_root.join("crates/lanekeep-rules/typescript/rolldown.config.mjs"))
        .arg("-o")
        .arg(components_dir.join("typescript-builtins.wasm"))
        .status()
        .unwrap_or_else(|e| {
            panic!("failed to run `jco componentize` to build `typescript-builtins.wasm`: {e}")
        });

    if !status.success() {
        panic!(
            "`jco componentize` failed to build `typescript-builtins.wasm` (the `just typescript-builtins` equivalent)"
        );
    }
}

/// Fail with an actionable message naming the missing tool and its `just` recipe
/// equivalent, and exit non-zero. A build script may panic to abort the build.
fn require_tool(tool: &str, recipe: &str) {
    if Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", tool))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return;
    }
    panic!(
        "error: required tool `{tool}` is not installed.\n\
         install it and re-run, or build the component with `{recipe}`."
    );
}

/// `println!` to stdout (cargo reads directives from the build script's stdout).
fn stdout(line: &str) {
    println!("{line}");
}
