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
use std::time::Duration;

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
    /// scenario; it is the observable — the engine counts a cache hit against the rule it
    /// would have consulted, so a rule's row still appears in stderr on a warm run, with
    /// `query`, `handler` and `matches` all at zero. `zero_work_row` builds that exact row,
    /// which is what distinguishes "the cache was read" from "the rule ran again and matched
    /// nothing" (both would leave `matches` at zero) and from "the rule was never admitted"
    /// (which the row's *presence* now rules out, where its *absence* used to be this test's
    /// whole claim).
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

    /// Change a component's bytes in place, without changing its behavior or its validity —
    /// the point being to simulate "the same rule, rebuilt" without a second committed
    /// fixture. Appends an inert custom section, which `wasm-tools validate --features
    /// component-model` accepts and `wasm-tools print` shows landing beside the artifact's own
    /// `version` section, verified by hand against `metadata.wasm` before this was written. A
    /// real rebuild changes bytes the same way — `hash_ruleset` folds bytes, not intent — so
    /// this is a faithful stand-in for one rather than a shortcut around what a rebuild does.
    fn append_custom_section(&self, at: &str, marker: &str) {
        let path = self.dir.join(at);
        let mut bytes = std::fs::read(&path).expect("the component is there to mutate");

        let name = b"lanekeep-test";
        let content = marker.as_bytes();
        let mut inner = leb128(name.len() as u64);
        inner.extend_from_slice(name);
        inner.extend_from_slice(content);

        bytes.push(0x00); // custom section id
        bytes.extend(leb128(inner.len() as u64));
        bytes.extend(inner);
        std::fs::write(&path, bytes).expect("writes");
    }
}

/// Unsigned LEB128, for the one custom-section length prefix `append_custom_section` writes.
/// Every value that method ever encodes with this fits in one byte, but spelling out the
/// general form is what lets a reader check it against the spec rather than trust that it does.
fn leb128(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
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

/// The exact row `write_profile` (`crates/lanekeep-cli/src/main.rs`) prints for a rule that
/// did no work this run: zero query time, zero handler time, zero matches.
///
/// A cache hit returns before either gate is consulted a second time, so a cache-hit rule's
/// `RuleTiming` for this run is `RuleTiming::default()` apart from the counters this crate
/// cannot see from stderr — `query`, `handler` and `matches` are exactly zero. Asserting on
/// this exact row rather than on mere presence of the rule's id is what tells "the cache was
/// read" apart from "the rule ran again and happened to match nothing" — a re-run rule's
/// query and handler durations are not reliably nonzero on a fast, tiny fixture, so `matches
/// == 0` alone would not distinguish the two. And asserting on this exact row rather than on
/// *absence* of the id — the property this test asserted before the engine started counting
/// cache hits — is what tells "cached" apart from "gated out and never admitted": both used
/// to look identical from stderr, and only one of them is the cache being read.
fn zero_work_row(id: &str) -> String {
    format!(
        "  {:<40} {:>9.1?} {:>9.1?} {:>9.1?} {:>9}",
        id,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        0
    )
}

/// A config naming one built-in TypeScript rule and one compiled component, in the one array a
/// real `lanekeep.json` mixes them in — `lanekeep-config`'s `json::rules_module` doc calls this
/// out as the case a placeholder index exists to keep sound.
///
/// **The TypeScript half is a Python-targeting rule, and that is what keeps the mix real.** It
/// was `lanekeep/no-default-export` until that rule was compiled into a component; leaving it
/// there would have made both entries components and quietly retired the case this fixture
/// exists for, with every assertion below still passing. Every built-in that is still evaluated
/// as a module targets Python, Go or Rust, so the subject file is Python.
const CONFIG: &str = r#"{
    "include": ["src/**"],
    "namespaces": ["fixture"],
    "rules": ["lanekeep/no-broad-except", "./rules/probe.wasm"]
}"#;

#[test]
fn a_component_backed_run_writes_and_reuses_its_cache() {
    // `caching: components.is_none()` turned the result cache off for any run loading a
    // component, because `RuleSpec::component` was set after `ruleset_hash` was computed. Now
    // that config fills it, the bytes reach the key and the run may cache.
    let project = Project::new(
        "reuse",
        &[
            // Violates `lanekeep/no-broad-except` — a real, visible answer to compare across
            // the two runs, so "reports identically" is not vacuously true of a rule that
            // never reports anything.
            ("src/a.py", "try:\n    run()\nexcept Exception:\n    pass\n"),
            // `metadata.wasm`'s gates: `path_matches: ["src/**/*.rs"]`, `file_contains:
            // ["call"]`, `file_not_contains: ["skip"]`. Its `check` reports nothing — see
            // `crates/lanekeep-wasm/tests/fixtures/metadata/src/lib.rs` — so it contributes no
            // violation, only a query that has to run on a cold pass and must not on a warm one.
            ("src/a.rs", "fn f() { call(); }\n"),
        ],
    );
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
    assert!(combined.contains("src/a.py"), "{combined}");

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
        cold_stderr.contains("lanekeep/no-broad-except"),
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
    // before either rule's gates are consulted a second time, so neither rule's query ran and
    // neither did any matching — but the engine now counts the hit itself, so each rule's row
    // still appears, showing zero work rather than being absent. See `zero_work_row`'s doc for
    // why a zero-work row is the stronger and more specific claim.
    assert!(
        warm_stderr.contains(&zero_work_row("fixture/metadata")),
        "a cache hit must still show the component rule's row, with zero work: {combined}"
    );
    assert!(
        warm_stderr.contains(&zero_work_row("lanekeep/no-broad-except")),
        "a cache hit must still show the TypeScript rule's row, with zero work: {combined}"
    );
}

/// The other direction the test above does not reach: a *configured* component's bytes
/// changing must invalidate every file's entry, not just fail to serve a stale one by luck.
///
/// `swapping_a_component_between_runs_changes_the_answer` (`lanekeep-engine`) is this
/// property's existing mirror, and says outright which half it does not cover: "(A component a
/// *config* names is folded into `ruleset_hash` by `lanekeep-config`; this path is the one that
/// is not.)" Every rule in that test is hand-built, so its `ComponentRule`s are never counted in
/// any `ruleset_hash` and the run never caches at all — there is nothing for a byte change to
/// invalidate. Here the component is configured, `ruleset_hash` covers its bytes, and the run
/// caches, so the property under test is real: change the bytes, and does the *next* run notice
/// on its own, the moment it happens, or does it take something extra?
///
/// It does not take anything extra. A fresh process re-reads `lanekeep.json` and the `.wasm`
/// beside it on every invocation — there is no warm state held across two runs of the CLI other
/// than the cache file itself — so `hash_ruleset` is recomputed from whatever bytes are on disk
/// *before* the cache is even consulted. The mechanism is the same one that already makes a
/// module edit invalidate a TypeScript rule's cache; this just exercises it for a component.
#[test]
fn a_configured_components_bytes_changing_forces_a_recompute() {
    let project = Project::new(
        "recompute",
        &[
            ("src/a.py", "try:\n    run()\nexcept Exception:\n    pass\n"),
            ("src/a.rs", "fn f() { call(); }\n"),
        ],
    );
    project.write("lanekeep.json", CONFIG);
    project.write_component("rules/probe.wasm", "metadata");

    let cold = project.check_profiled();
    assert_eq!(cold.status.code(), Some(1), "{}", describe(&cold));
    let cold_stderr = String::from_utf8_lossy(&cold.stderr).into_owned();
    assert!(
        cold_stderr.contains("fixture/metadata")
            && cold_stderr.contains("lanekeep/no-broad-except"),
        "both rules should have run cold: {}",
        describe(&cold)
    );

    let warm = project.check_profiled();
    let warm_stderr = String::from_utf8_lossy(&warm.stderr).into_owned();
    assert!(
        warm_stderr.contains(&zero_work_row("fixture/metadata"))
            && warm_stderr.contains(&zero_work_row("lanekeep/no-broad-except")),
        "the second run over unchanged input should have been a full cache hit — both rows          should show zero work rather than being absent or re-run: {}",
        describe(&warm)
    );

    // The rule's own answers — its id, query, card, gates — come from `metadata()`, which this
    // does not touch. What changes is the code the rule is *made of*, exactly as editing a
    // TypeScript rule's source does, and `ruleset_hash` is defined to cover that.
    project.append_custom_section("rules/probe.wasm", "first-mutation");

    let recomputed = project.check_profiled();
    let combined = describe(&recomputed);
    assert_eq!(recomputed.status.code(), cold.status.code(), "{combined}");
    assert_eq!(
        String::from_utf8_lossy(&recomputed.stdout),
        String::from_utf8_lossy(&cold.stdout),
        "the same rules over the same source should still report the same violations: {combined}"
    );
    let recomputed_stderr = String::from_utf8_lossy(&recomputed.stderr).into_owned();
    // The one assertion this test exists for. `caching` is a per-*run* flag — see
    // `Engine::caching`'s doc on why there is no finer granularity that is sound — so a moved
    // `ruleset_hash` invalidates every file's entry in this run, whether or not that file's own
    // rule is the component that changed. Both rows have to reappear, not just the component's.
    assert!(
        recomputed_stderr.contains("fixture/metadata"),
        "the component did not run again after its own bytes changed: {combined}"
    );
    assert!(
        recomputed_stderr.contains("lanekeep/no-broad-except"),
        "the TypeScript rule did not run again after the component's bytes changed — the \
         whole run's key should have moved, not just the changed rule's: {combined}"
    );

    // And the new bytes are themselves cacheable — this is what shows the recompute above
    // actually wrote a fresh entry rather than merely refusing to serve a stale one. A design
    // that invalidated correctly but then failed to re-cache would pass every assertion above
    // and run cold forever after.
    let warm_again = project.check_profiled();
    let combined = describe(&warm_again);
    let warm_again_stderr = String::from_utf8_lossy(&warm_again.stderr).into_owned();
    assert!(
        warm_again_stderr.contains(&zero_work_row("fixture/metadata"))
            && warm_again_stderr.contains(&zero_work_row("lanekeep/no-broad-except")),
        "the run after the recompute should have warmed on the new bytes — both rows should          show zero work: {combined}"
    );
}
