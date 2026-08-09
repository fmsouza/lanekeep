//! A component-backed run caches its results, end to end through a real `lanekeep.json`.
//!
//! `Engine::caching` used to be `components.is_none()` — off for *any* run that loaded a
//! component, because a component's bytes reached no cache-key input at all. `lanekeep-config`
//! now resolves a `.wasm` reference itself and folds its bytes into `ruleset_hash` before a
//! `Config` exists, so a run whose component arrived that way may cache. Nothing below the CLI
//! walks `lanekeep.json` → read → hash → execute in one piece, which is why this drives the real
//! binary rather than `Engine::prepare` directly — see `crates/lanekeep-engine/src/lib.rs`'s
//! `Engine::caching` field doc for the full account, including the case that still must not
//! cache.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A project on disk, checked by shelling out to the real binary.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-component-cache-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates dir");

        let project = Self { dir };
        for (path, contents) in files {
            project.write(path, contents);
        }
        project
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    /// Copy a committed `lanekeep-wasm` fixture in as this project's rule component.
    ///
    /// By path rather than `include_bytes!`, matching `lanekeep-config`'s and
    /// `lanekeep-engine`'s own tests: a compile-time include would make this crate fail to
    /// build for anyone who vendored it without `lanekeep-wasm`'s `tests/` tree.
    fn write_component(&self, at: &str, fixture: &str) {
        let from = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../lanekeep-wasm/tests/fixtures")
            .join(format!("{fixture}.wasm"));
        let bytes = std::fs::read(&from).expect("the fixture ships");
        let full = self.dir.join(at);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, bytes).expect("writes");
    }

    /// Run `check --profile` and return the result. `--profile` is not the point of the
    /// scenario; it is the observable, because a rule's id appears in its stderr table only for
    /// a file whose gates and query actually ran this run — a cache hit returns before either,
    /// so a component that never left `--profile`'s report is a component that did not execute.
    fn check_profiled(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg("--profile")
            .arg(&self.dir)
            .output()
            .expect("runs the binary")
    }

    fn cache_exists(&self) -> bool {
        self.dir.join(".lanekeep").join("cache").exists()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn describe(output: &Output) -> String {
    format!(
        "exit: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// A config naming one built-in TypeScript rule and one compiled component, in the one array a
/// real `lanekeep.json` mixes them in — `lanekeep-config`'s `json::rules_module` doc calls this
/// out as the case a placeholder index exists to keep sound.
const CONFIG: &str = r#"{
    "include": ["src/**"],
    "namespaces": ["fixture"],
    "rules": ["lanekeep/no-default-export", "./rules/probe.wasm"]
}"#;

#[test]
fn a_component_backed_run_writes_and_reuses_its_cache() {
    // `caching: components.is_none()` turned the result cache off for any run loading a
    // component, because `RuleSpec::component` was set after `ruleset_hash` was computed. Now
    // that config fills it, the bytes reach the key and the run may cache.
    let project = Project::new(
        "reuse",
        &[
            // Violates `lanekeep/no-default-export` — a real, visible answer to compare across
            // the two runs, so "reports identically" is not vacuously true of a rule that
            // never reports anything.
            ("src/a.ts", "export default 1;\n"),
            // `metadata.wasm`'s gates: `path_matches: ["src/**/*.rs"]`, `file_contains:
            // ["call"]`, `file_not_contains: ["skip"]`. Its `check` reports nothing — see
            // `crates/lanekeep-wasm/tests/fixtures/metadata/src/lib.rs` — so it contributes no
            // violation, only a query that has to run on a cold pass and must not on a warm one.
            ("src/a.rs", "fn f() { call(); }\n"),
        ],
    );
    project.write(".gitignore", "");
    project.write("lanekeep.json", CONFIG);
    project.write_component("rules/probe.wasm", "metadata");

    assert!(
        !project.cache_exists(),
        "a cache exists before the first run"
    );

    let cold = project.check_profiled();
    let combined = describe(&cold);
    // `--profile` writes to stderr for exactly this reason — `profile_goes_to_stderr_so_json_
    // still_pipes` in `init_and_profile.rs` — so it has to be read separately from stdout. The
    // rule id legitimately appears in stdout too, as part of the ordinary violation message, on
    // *both* a cold and a warm run: a cache hit still carries the `RuleId` that reported it. So
    // stdout can prove "reported identically" and nothing about whether a query ran, and stderr
    // is the only half that can.
    let cold_stderr = String::from_utf8_lossy(&cold.stderr).into_owned();
    assert_eq!(cold.status.code(), Some(1), "{combined}");
    assert!(combined.contains("src/a.ts"), "{combined}");

    // The direct regression check: caching used to be off for *any* run that loaded a
    // component, configured or not, so this file would never exist. `metadata` is configured —
    // resolved by `lanekeep-config` from `lanekeep.json`, its bytes folded into `ruleset_hash`
    // before this run's `Config` existed — so it must exist now.
    assert!(
        project.cache_exists(),
        "a component-backed run wrote no cache: {combined}"
    );

    // Both rules ran on the cold pass: neither file could have been a cache hit before either
    // rule's query had a chance to run once. `matches` is not asserted as a number — only that
    // the row exists at all, which `a_rule_that_never_matched_still_appears`
    // (`lanekeep-engine`) already establishes does not require a match, only that the query ran.
    assert!(
        cold_stderr.contains("lanekeep/no-default-export"),
        "the TypeScript rule's row is missing from a cold profile: {combined}"
    );
    assert!(
        cold_stderr.contains("fixture/metadata"),
        "the component rule's row is missing from a cold profile: {combined}"
    );

    let warm = project.check_profiled();
    let combined = describe(&warm);
    let warm_stderr = String::from_utf8_lossy(&warm.stderr).into_owned();
    assert_eq!(warm.status.code(), cold.status.code(), "{combined}");
    assert_eq!(
        String::from_utf8_lossy(&warm.stdout),
        String::from_utf8_lossy(&cold.stdout),
        "the second run reported differently: {combined}"
    );

    // The proof that the second run actually *read* the cache rather than recomputing an
    // identical answer — which a correct uncached run would also produce, and which is exactly
    // the assertion the brief warns passes against this bug. `check_file` returns a cache hit
    // before either rule's gates are consulted a second time, so neither rule's query ran, so
    // neither rule's id was ever handed to `--profile` to report. Absence here is the whole
    // claim; it cannot be produced by a slower recompute that happens to agree.
    assert!(
        !warm_stderr.contains("fixture/metadata"),
        "the component rule ran again on a warm pass — the cache was not read: {combined}"
    );
    assert!(
        !warm_stderr.contains("lanekeep/no-default-export"),
        "the TypeScript rule ran again on a warm pass — the cache was not read: {combined}"
    );
}
