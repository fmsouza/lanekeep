//! A declaration edit mid-session recomputes exactly its importers — under `builtin` — and
//! the whole program — under `tsc`.
//!
//! One provider, two runs. That is what makes this a session rather than two invocations:
//! driving the binary twice builds a fresh provider each time, so nothing about *held* state
//! would be exercised at all.
//!
//! **What it does not pin is `revalidate`, and saying so is the point.** A provider that
//! revalidated nothing passes every assertion below, and correctly: the builtin provider's
//! `declaration()` compares each held parse against the file's hash on every access, and
//! `begin_run` clears the completeness memo wholesale, so a stale entry can never be served
//! whether or not `revalidate` ran. `revalidate` reclaims the memory a dropped entry holds and
//! has no effect on any answer. What this file pins is the pair either provider has to get
//! right regardless: the second run sees the edit, and the result cache recomputes exactly the
//! entries that depended on it — its importers under `builtin`, and every file under `tsc`,
//! for the reason below.
//!
//! `cached` is read from `Outcome.timings`, which only `Engine::profiling()` populates. It is
//! per rule and per file, and each row reconciles: `path_gated + unread + cached +
//! content_gated + language_gated + parsed` equals the discovered count. This fixture's rule
//! has no gates, so every discovered file lands in `cached` or `parsed` and the two numbers
//! say exactly which files were recomputed.
//!
//! The sequence a run drives — `provider.revalidate(&FileAccess::new(dir))`, then
//! `Engine::prepare_with_provider` with the same handle — is `SessionProvider::for_request_with`
//! opened up (`crates/lanekeep-cli/src/session.rs`): revalidate while the lock is not held,
//! then hand the provider to `prepare`. `SessionProvider` itself is `pub(crate)` and reachable
//! only from `lanekeep-cli`'s own binary target, not from an integration test in this package,
//! so this drives that sequence directly rather than through it — the two are required to stay
//! in step, and `run_with`'s doc comment says so.
//!
//! # `tsc`: one hash over the whole program, not a per-file dependency
//!
//! `docs/architecture.md` §8.1: "`tsc`'s results are whole-program: a `.d.ts` edit anywhere can
//! change the type of an expression anywhere else, so a per-entry dependency list would run to
//! hundreds of files ... One hash over the whole program set, folded into the run key,
//! invalidates everything on any change instead." So under `tsc` the declaration edit does not
//! recompute only its importers — it recomputes every file, importer and non-importer alike,
//! because the edit changes the one key every file's cache entry was written under. The two
//! providers therefore assert opposite counts from the same edit, which is the point of running
//! both: a test that only exercised `builtin` would leave architecture's whole-program claim
//! unverified.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below, and the `corpus` helpers this file \
              also compiles, are neither, so the grant it already makes for unit tests has to \
              be restated for them."
)]
#![expect(
    clippy::print_stderr,
    reason = "the `tsc` case has to say why it did nothing when `typescript` is absent: the \
              alternative is a suite reporting a pass for a test it did not run"
)]

mod corpus;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use corpus::tsc_available;
use lanekeep_core::FileAccess;
use lanekeep_engine::Engine;
use lanekeep_lang_js::{JavaScript, Tsx, TypeScript};
use lanekeep_types::TypeProvider;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// This repository's own authoring `typescript`, absolute and forward-slashed.
///
/// Spelled with forward slashes: `validate_specifier` and JSON escaping both have history
/// with backslashes on Windows, and Rust accepts either separator there, so `C:/Users/...`
/// is still absolute and carries nothing either gate refuses — the same reasoning
/// `server_agreement.rs`'s copy of this helper documents.
fn typescript_package() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/lanekeep/node_modules/typescript")
        .canonicalize()
        .expect("the authoring package is installed")
        .to_string_lossy()
        .replace('\\', "/")
}

const RULE: &str = r"import { defineRule } from 'lanekeep'

export default defineRule({
  id: 'local/no-number-money',
  severity: 'error',
  requires: ['types'],
  card: {
    message: 'a money value must not be a bare number',
    remediation: 'give it the Money type the package exports',
    examples: { bad: 'const total = rate', good: 'const total: Money = rate' },
  },
  query: '(variable_declarator name: (identifier) @name value: (identifier) @value)',
  check(ctx, m) {
    const type = ctx.types.typeOf(m.value)
    if (type === undefined) return
    if (type.primitive !== 'number') return
    ctx.report(m.name)
  },
})
";

const PACKAGE_JSON: &str = r#"{
  "name": "@acme/rates",
  "version": "1.0.0",
  "types": "index.d.ts"
}
"#;

struct Tree {
    dir: PathBuf,
}

impl Tree {
    /// Three source files: two that import the declaration and one that does not.
    ///
    /// The third is what makes "exactly its importers" an assertion rather than a hope — with
    /// only importers in the tree, "everything recomputed" and "the importers recomputed" are
    /// the same number. It is also what makes the `tsc` half meaningful the other way: with a
    /// non-importer in the tree, "everything recomputed" is a claim that can fail.
    fn new(name: &str, provider: &str) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-invalidate-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let tree = Self { dir };
        tree.write("package.json", "{}\n");
        tree.write("lanekeep/rules/no-number-money.ts", RULE);
        tree.write("node_modules/@acme/rates/package.json", PACKAGE_JSON);
        tree.write(
            "src/a.ts",
            "import { rate } from '@acme/rates'\nexport const total = rate\n",
        );
        tree.write(
            "src/b.ts",
            "import { rate } from '@acme/rates'\nexport const fee = rate\n",
        );
        tree.write("src/c.ts", "export const n = 1\n");

        // `builtin` needs nothing extra; `tsc` also needs `types.typescript`, pointed at this
        // repository's own authoring package by absolute path — the fixture tree is a
        // throwaway directory in the system temp with no `node_modules` of its own.
        let types_block = if provider == "tsc" {
            format!(
                r#"{{"provider": "tsc", "typescript": "{}"}}"#,
                typescript_package()
            )
        } else {
            format!(r#"{{"provider": "{provider}"}}"#)
        };
        tree.write(
            "lanekeep.json",
            &format!(
                r#"{{"include": ["src/**"],
 "timeouts": {{"rule": 600000, "global": 600000}},
 "types": {types_block},
 "rules": ["./lanekeep/rules/no-number-money.ts"]}}
"#
            ),
        );
        tree
    }

    fn write(&self, path: &str, contents: &str) {
        let full = self.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("creates parent");
        }
        std::fs::write(full, contents).expect("writes");
    }

    fn remove(&self, path: &str) {
        let _ = std::fs::remove_file(self.dir.join(path));
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What one run of the held provider produced: violations, and files served from cache.
struct Run {
    violations: Vec<String>,
    cached: u64,
    parsed: u64,
}

/// One run over `dir`, using `provider` — the same handle across every call.
///
/// This is `SessionProvider::for_request_with` opened up (see the module doc): every argument
/// matches what that function drives, in the same order — revalidate, then prepare with the
/// held handle. If it stops matching, this test is measuring something else.
fn run_with(dir: &Path, provider: &Arc<dyn TypeProvider>) -> Run {
    let root = lanekeep_js::RuleRoot::new(dir)
        .expect("a usable root")
        .with_builtins(lanekeep_rules::source)
        .with_builtin_components(lanekeep_rules::component)
        .with_builtin_component_maps(lanekeep_rules::component_source_map)
        .with_builtin_component_declared(lanekeep_rules::is_declared_component);
    let config_path = dir.join("lanekeep.json");

    let sandbox = lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
        .expect("a sandbox");
    let loaded = lanekeep_config::load_with(
        &sandbox,
        &root,
        &config_path,
        lanekeep_config::LoadOptions {
            artifacts: Some(dir),
            global_timeout: None,
        },
    )
    .expect("loads the config");

    // What a session does before every request.
    provider.revalidate(&FileAccess::new(dir));

    let engine = Engine::prepare_with_provider(
        &loaded,
        dir,
        root,
        &config_path,
        &lanekeep_languages::registry(),
        Arc::new(TypeScript),
        Arc::new(JavaScript),
        Some(Arc::clone(provider)),
        lanekeep_engine::PrepareOptions::default(),
    )
    .expect("prepares")
    .profiling();

    let outcome = engine.run().expect("runs");
    let timings = outcome.timings.expect("profiling was asked for");
    let timing = timings
        .values()
        .next()
        .expect("one rule, therefore one row");

    Run {
        violations: outcome
            .violations
            .iter()
            .map(|v| format!("{}:{}", v.location.file, v.location.position.line))
            .collect(),
        cached: timing.cached,
        parsed: timing.parsed,
    }
}

/// The provider a session would build for this fixture, held across every run below.
fn held(dir: &Path) -> Arc<dyn TypeProvider> {
    let root = lanekeep_js::RuleRoot::new(dir).expect("a usable root");
    let sandbox = lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
        .expect("a sandbox");
    let loaded = lanekeep_config::load_with(
        &sandbox,
        &root,
        &dir.join("lanekeep.json"),
        lanekeep_config::LoadOptions {
            artifacts: Some(dir),
            global_timeout: None,
        },
    )
    .expect("loads the config");
    lanekeep_engine::provider_for(
        &loaded.types,
        dir,
        Some(&TypeScript),
        // The second grammar beside the first, the way `SessionProvider::for_request` and the
        // engine's own run path both pass them — the fixture's importer is a `.ts` file, so
        // the grammar pair itself is not what this pins, only that it is the pair a session
        // would hold.
        Some(&Tsx),
        lanekeep_core::AnalysisBudget::start(loaded.limits.analysis_timeout),
    )
    .expect("builds a provider")
}

#[test]
fn a_declaration_edit_recomputes_exactly_its_importers() {
    let tree = Tree::new("edit", "builtin");
    tree.write(
        "node_modules/@acme/rates/index.d.ts",
        "export declare const rate: number\n",
    );
    let provider = held(&tree.dir);

    let cold = run_with(&tree.dir, &provider);
    assert_eq!(cold.cached, 0, "nothing is cached on a cold run");
    assert_eq!(cold.parsed, 3);
    assert_eq!(cold.violations, ["src/a.ts:2", "src/b.ts:2"]);

    tree.write(
        "node_modules/@acme/rates/index.d.ts",
        "export declare const rate: string\n",
    );

    let warm = run_with(&tree.dir, &provider);
    assert_eq!(
        warm.cached, 1,
        "only the file that never read the declaration is served warm"
    );
    assert_eq!(
        warm.parsed, 2,
        "and exactly the two importers are recomputed"
    );
    assert!(
        warm.violations.is_empty(),
        "the type is a string now: {:?}",
        warm.violations
    );
}

#[test]
fn a_declaration_appearing_invalidates_the_importers_that_recorded_its_absence() {
    // Absence is a dependency (architecture §8.2), and it is the half that fails silently:
    // adding a file changes nothing until something unrelated invalidates the entry, so a
    // rule that was correctly quiet stays quiet after the answer changed. The held provider
    // has the same hazard one layer up, in the misses it memoized.
    let tree = Tree::new("appears", "builtin");
    tree.remove("node_modules/@acme/rates/index.d.ts");
    let provider = held(&tree.dir);

    let cold = run_with(&tree.dir, &provider);
    assert_eq!(cold.cached, 0);
    assert!(
        cold.violations.is_empty(),
        "with no declaration the oracle is unsure and the rule is silent: {:?}",
        cold.violations
    );

    tree.write(
        "node_modules/@acme/rates/index.d.ts",
        "export declare const rate: number\n",
    );

    let warm = run_with(&tree.dir, &provider);
    assert_eq!(
        warm.cached, 1,
        "the non-importer is untouched by a file appearing"
    );
    assert_eq!(warm.parsed, 2);
    assert_eq!(
        warm.violations,
        ["src/a.ts:2", "src/b.ts:2"],
        "the memoized miss was re-probed and the entries that recorded it were invalidated"
    );
}

#[test]
fn under_tsc_a_declaration_edit_recomputes_the_whole_program() {
    if !tsc_available() {
        eprintln!(
            "skipped: no `packages/lanekeep/node_modules/typescript`; run `npm ci` in \
             packages/lanekeep, or read this on the Linux gate job"
        );
        return;
    }

    let tree = Tree::new("tsc-edit", "tsc");
    tree.write(
        "node_modules/@acme/rates/index.d.ts",
        "export declare const rate: number\n",
    );
    let provider = held(&tree.dir);

    let cold = run_with(&tree.dir, &provider);
    assert_eq!(cold.cached, 0, "nothing is cached on a cold run");
    assert_eq!(cold.parsed, 3);
    assert_eq!(cold.violations, ["src/a.ts:2", "src/b.ts:2"]);

    tree.write(
        "node_modules/@acme/rates/index.d.ts",
        "export declare const rate: string\n",
    );

    let warm = run_with(&tree.dir, &provider);
    assert_eq!(
        warm.cached, 0,
        "under `tsc` the program listing is one hash over every file, so the non-importer \
         is recomputed too, not served warm — architecture §8.2"
    );
    assert_eq!(
        warm.parsed, 3,
        "all three files, not only the two importers"
    );
    assert!(
        warm.violations.is_empty(),
        "the type is a string now: {:?}",
        warm.violations
    );
}
