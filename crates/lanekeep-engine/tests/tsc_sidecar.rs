//! The `tsc` sidecar dies with the engine that spawned it.
//!
//! **Why this is an integration test rather than a `#[cfg(test)]` module beside the engine.**
//! Answering "is that process still running?" needs `pgrep`, and `local/no-ambient-authority`
//! — lanekeep's own self-check, run by `just lanekeep` — refuses `std::process::Command`
//! anywhere in the engine's sources but `crates/lanekeep-core/src/changed.rs`. Its exemption
//! for scaffolding is a path: a file under a `tests/` directory. That is the rule working as
//! designed, since §13's claim is about what the engine does in a run and not about the
//! harness that proves it, and a `#[cfg(test)]` module inside `src/lib.rs` is on the wrong
//! side of a check that can only see paths. So the test moves rather than the rule.
//!
//! Its sibling — the one asserting a `tsc` run's programs reach the key it checks files under
//! — stays in `src/lib.rs`, because it reads `Engine`'s private `run_key`. Widening that field
//! to move a test would trade a real boundary for a cosmetic one.
//!
//! Everything here needs the authoring package's `typescript` and says so on the terminal when
//! it is absent: a suite reporting passes for tests it did not run is worse than a short one.

#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules, and the helpers below are neither."
)]
#![expect(
    clippy::print_stderr,
    reason = "a test that finds `typescript` absent has to say so on the terminal: the \
              alternative is a suite that reports a pass for a test it did not run"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lanekeep_config::TypesProvider;
use lanekeep_engine::Engine;
use lanekeep_js::RuleRoot;
use lanekeep_lang_js::{JavaScript, TypeScript};

/// A rule reporting every `debugger` statement — small, unambiguous, and easy to seed.
///
/// The same fixture rule `src/lib.rs`'s test module uses, copied rather than shared: a
/// `#[cfg(test)]` item is not reachable from an integration test, and the alternative is
/// making a test constant part of the crate's public surface.
const DEBUGGER_RULE: &str = "import { defineRule } from 'lanekeep';\n\
    export default defineRule({\n\
      id: 'local/no-debugger',\n\
      query: '(debugger_statement) @stmt',\n\
      card: {\n\
        message: 'debugger statement',\n\
        remediation: 'remove it before committing',\n\
        examples: { bad: 'debugger;', good: 'console.log(x);' },\n\
      },\n\
      check(ctx, m) { ctx.report(m.stmt); },\n\
    });\n";

/// A throwaway project on disk, removed when it drops.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("lanekeep-engine-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates the project directory");
        let project = Self { dir };
        for (path, contents) in files {
            let full = project.dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("creates the parent directory");
            }
            std::fs::write(full, contents).expect("writes the fixture file");
        }
        project
    }

    /// The engine over the fixture's config, without running it.
    fn prepare(&self) -> Engine {
        let root = RuleRoot::new(&self.dir).expect("canonicalizes");
        let config_path = self.dir.join("lanekeep.config.ts");
        let sandbox =
            lanekeep_config::sandbox_for(&root, Arc::new(TypeScript), Arc::new(JavaScript))
                .expect("builds the sandbox");
        let config =
            lanekeep_config::load(&sandbox, &root, &config_path).expect("the config loads");
        Engine::prepare(
            &config,
            &self.dir,
            root,
            &config_path,
            &lanekeep_languages::registry(),
            Arc::new(TypeScript),
            Arc::new(JavaScript),
        )
        .expect("the sidecar starts and its programs build")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The absolute path of this repository's own `typescript`, for a fixture's `types` block.
///
/// Forward slashes: the path is interpolated into a TypeScript config, where a backslash opens
/// an escape (`AGENTS.md`).
fn typescript_package() -> Option<String> {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/lanekeep/node_modules/typescript");
    package
        .join("package.json")
        .is_file()
        .then(|| package.canonicalize())?
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

/// Whether anything on this machine is running against `dir`.
///
/// `pgrep -f` matches an *extended regular expression*, not a literal, and the project root is
/// not one to trust as a pattern: on macOS `std::env::temp_dir()` is `/var/folders/xy/z+w.../T/`,
/// whose `+` is a repetition operator. A pattern that fails to compile makes `pgrep` exit
/// non-zero, which this reads as "no sidecar" — so the assertion that one is *alive* would fail
/// for a reason that has nothing to do with the sidecar, and the assertion that one is *gone*
/// would pass without ever looking.
///
/// The directory's own last component carries no metacharacter: it is `lanekeep-engine-<name>`,
/// built by `Project::new` from a literal this file writes. It is still unique to this project,
/// which is what the match needs — the sidecar carries it twice on its command line, since the
/// driver it runs lives under `<root>/.lanekeep/` and the root is its one argument.
fn sidecar_alive(dir: &Path) -> bool {
    let token = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .expect("the fixture's directory has a name");
    assert!(
        token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "`{token}` would be read as a regular expression by `pgrep -f`"
    );
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(&token)
        .output()
        .is_ok_and(|out| out.status.success())
}

#[test]
fn the_sidecar_dies_with_the_engine() {
    let Some(typescript) = typescript_package() else {
        eprintln!("skipped: no packages/lanekeep/node_modules/typescript (CI covers this)");
        return;
    };
    let project = Project::new(
        "tsc-provider-drop",
        &[
            ("rule.ts", DEBUGGER_RULE),
            (
                "lanekeep.config.ts",
                &format!(
                    "import {{ defineConfig }} from 'lanekeep';\n\
                     import rule from './rule';\n\
                     export default defineConfig({{ include: ['src/**/*.ts'], rules: [rule], \
                     types: {{ provider: 'tsc', typescript: '{typescript}' }} }});\n"
                ),
            ),
            ("package.json", "{\"name\":\"f\",\"private\":true}\n"),
            (
                "tsconfig.json",
                "{\"compilerOptions\":{\"strict\":true},\"include\":[\"src\"]}\n",
            ),
            ("src/a.ts", "export function a() {\n  debugger;\n}\n"),
        ],
    );

    let engine = project.prepare();
    assert_eq!(
        engine.types_provider(),
        TypesProvider::Tsc,
        "the config asked for `tsc`"
    );
    assert!(
        sidecar_alive(&project.dir),
        "a prepared `tsc` run holds a live sidecar"
    );

    let outcome = engine.run().expect("the run completes");
    assert_eq!(outcome.violations.len(), 1, "one `debugger` statement");

    // The point of the test. `TscProvider`'s `Drop` kills and reaps the child, and the provider
    // is dropped with the engine — so a run that ended leaves nothing holding a compiler's
    // worth of state.
    drop(engine);
    assert!(
        !sidecar_alive(&project.dir),
        "the sidecar outlived the engine that spawned it"
    );
}
