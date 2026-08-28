//! Rebuilds the shipped WebAssembly components from source when their artifacts are missing.
//!
//! Three artifacts are shipped and embedded via `include_bytes!`: `go-builtins.wasm`,
//! `no-glob-import.wasm` and `no-unwrap.wasm`. They are read with `include_bytes!`, so a normal
//! build — and, critically, `cargo publish`'s verify build, where the sources these were built
//! from are absent from the package — must be a strict no-op. Only when an artifact is actually
//! missing do we reach for a toolchain, and only then do we emit `rerun-if-changed` directives,
//! so a checkout that has everything never re-runs the script and a package that has only the
//! three artifacts never attempts a rebuild.
//!
//! `typescript-builtins.wasm` is no longer shipped — the four TypeScript built-ins run as
//! QuickJS modules (see `src/lib.rs`'s `BUILT_IN_RULES`). It is still built when missing so
//! that tests and benchmarks that `include_bytes!` it (in `lanekeep-wasm/tests/world_shape.rs`
//! and `lanekeep-rules/tests/source_maps.rs`) can compile, but it is not embedded in the
//! published binary and is excluded from the published `.crate`.
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

    // Existence is not currency: an artifact built against an older `world.wit` still
    // exists, and a world edit that changes an export's shape makes every such artifact fail
    // at prepare — as a component-type mismatch classified as the *rule's* failure, naming
    // neither staleness nor the file. `rerun-if-changed` re-runs this script on a world
    // edit; the stamp below is what makes the re-run act. The stamp is the world's own
    // bytes, and the check runs only when the world is readable — in `cargo publish`'s
    // verify build the sources are absent from the package, the read fails, and this stays
    // the strict no-op the module documentation requires.
    //
    // Staleness *attempts* a rebuild, and a failed attempt keeps the old artifact behind a
    // `cargo:warning`, where a *missing* artifact's failed build still aborts. The
    // difference is what there is to fall back to: with no artifact the crate cannot compile
    // at all, while a stale one compiles and only the component-running tests disagree with
    // it — and a machine that cannot rebuild (no TinyGo, or a TinyGo refusing the host Go)
    // must not be bricked by a world edit it can do nothing about locally.
    let stamp = components_dir.join(".world-wit.stamp");
    let world = fs::read(&world_wit).ok();
    let stale = world
        .as_ref()
        .is_some_and(|world| !fs::read(&stamp).is_ok_and(|recorded| &recorded == world));
    let mut refreshed_all = true;

    let refresh = |present: bool, build: &dyn Fn() -> Result<(), String>, recipe: &str| {
        if present && !stale {
            return true;
        }
        match build() {
            Ok(()) => true,
            Err(message) if !present => panic!("{message}"),
            Err(message) => {
                stdout(&format!(
                    "cargo:warning=a shipped component was built against an older \
                         `world.wit` and could not be rebuilt: {message}"
                ));
                stdout(&format!(
                    "cargo:warning=the previous artifact is kept; component-running \
                         tests will fail against it until `{recipe}` succeeds"
                ));
                false
            }
        }
    };

    refreshed_all &= refresh(
        components_dir.join("go-builtins.wasm").exists(),
        &|| build_go_builtins(&repo_root, &components_dir),
        "just go-rules",
    );
    refreshed_all &= refresh(
        components_dir.join("no-glob-import.wasm").exists(),
        &|| build_rust_component(&repo_root, &components_dir, "no-glob-import"),
        "just rust-rules",
    );
    refreshed_all &= refresh(
        components_dir.join("no-unwrap.wasm").exists(),
        &|| build_rust_component(&repo_root, &components_dir, "no-unwrap"),
        "just rust-rules",
    );
    // The TypeScript component ships with its source map sidecar, and `lib.rs` embeds both — so
    // either missing is a missing artifact, and the check must match what `include_bytes!` reads.
    refreshed_all &= refresh(
        components_dir.join("typescript-builtins.wasm").exists()
            && components_dir.join("typescript-builtins.wasm.map").exists(),
        &|| build_typescript_builtins(&repo_root, &components_dir),
        "just typescript-builtins",
    );

    // Recorded only once every artifact is current, so a kept-stale artifact leaves the
    // stamp stale and the warning firing on every build until a rebuild lands.
    if refreshed_all
        && let Some(world) = world
        && !fs::read(&stamp).is_ok_and(|recorded| recorded == world)
    {
        fs::write(&stamp, world)
            .unwrap_or_else(|e| panic!("failed to write `{}`: {e}", stamp.display()));
    }
}

/// Rebuild `go-builtins.wasm` with TinyGo, reproducing `just go-rules`.
///
/// Fallible rather than panicking, because the caller decides what a failure means: fatal
/// for a missing artifact, a kept-stale warning for a rebuild.
fn build_go_builtins(repo_root: &Path, components_dir: &Path) -> Result<(), String> {
    // `tinygo build` also invokes `wasm-opt` (WASMOPT) and `wasm-tools` internally, but the
    // recipe's own guard is `tinygo`; the failure of those two surfaces from inside the build
    // with the toolchain naming them.
    require_tool("tinygo", "just go-rules")?;

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
        .map_err(|e| format!("failed to run `tinygo build` to build `go-builtins.wasm`: {e}"))?;

    if !status.success() {
        return Err(
            "`tinygo build` failed to build `go-builtins.wasm` (the `just go-rules` equivalent)"
                .to_owned(),
        );
    }
    Ok(())
}

/// Rebuild one of the two shipped Rust components with `cargo component`, reproducing
/// `just rust-rules` for that one crate. `name` is the directory name with hyphens, e.g.
/// `no-unwrap`; cargo names the artifact with hyphens turned into underscores.
fn build_rust_component(repo_root: &Path, components_dir: &Path, name: &str) -> Result<(), String> {
    require_tool("cargo-component", "just rust-rules")?;

    let underscore_name = name.replace('-', "_");

    let status = Command::new("cargo")
        .arg("component")
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .current_dir(repo_root.join("rust-rules").join(name))
        .status()
        .map_err(|e| {
            format!("failed to run `cargo component build` to build `{name}.wasm`: {e}")
        })?;

    if !status.success() {
        return Err(format!(
            "`cargo component build` failed to build `{name}.wasm` (the `just rust-rules` equivalent)"
        ));
    }

    fs::copy(
        repo_root
            .join("rust-rules/target/wasm32-unknown-unknown/release")
            .join(format!("{underscore_name}.wasm")),
        components_dir.join(format!("{name}.wasm")),
    )
    .map_err(|e| format!("failed to copy the built `{name}.wasm` into components/: {e}"))?;
    Ok(())
}

/// Rebuild `typescript-builtins.wasm` (and its `.map`) with `jco componentize`,
/// reproducing `just typescript-builtins` (which routes through `_componentize`).
fn build_typescript_builtins(repo_root: &Path, components_dir: &Path) -> Result<(), String> {
    require_tool("node", "just typescript-builtins")?;
    require_tool("npx", "just typescript-builtins")?;

    let jco = repo_root.join("packages/lanekeep/node_modules/.bin/jco");
    if !jco.is_file() {
        return Err("error: 'packages/lanekeep' has no installed jco.\n\
             run `npm --prefix packages/lanekeep ci` first, or run `just typescript-builtins`."
            .to_owned());
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
        .map_err(|e| {
            format!("failed to run `jco componentize` to build `typescript-builtins.wasm`: {e}")
        })?;

    if !status.success() {
        return Err(
            "`jco componentize` failed to build `typescript-builtins.wasm` (the `just typescript-builtins` equivalent)"
                .to_owned(),
        );
    }
    Ok(())
}

/// An actionable refusal naming the missing tool and its `just` recipe equivalent.
///
/// A `Result` rather than a panic, so the caller decides whether a missing tool aborts the
/// build (a missing artifact) or becomes a kept-stale warning (a failed refresh).
fn require_tool(tool: &str, recipe: &str) -> Result<(), String> {
    if Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", tool))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(());
    }
    Err(format!(
        "error: required tool `{tool}` is not installed.\n\
         install it and re-run, or build the component with `{recipe}`."
    ))
}

/// `println!` to stdout (cargo reads directives from the build script's stdout).
fn stdout(line: &str) {
    println!("{line}");
}
