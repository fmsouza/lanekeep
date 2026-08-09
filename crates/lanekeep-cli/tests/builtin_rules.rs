//! The built-in rules, reached the way a user reaches them: by specifier, through the
//! binary.
//!
//! The rule crate's own tests hand the engine a rule's source directly, which proves the
//! rules are correct and proves nothing about whether anyone can get at them. This file
//! covers the wiring in between — that `import from 'lanekeep/<name>'` resolves at all, and
//! that a project cannot shadow it.

#![expect(
    clippy::expect_used,
    reason = "`clippy.toml`'s allow-*-in-tests only reaches `#[test]` functions and \
              `#[cfg(test)]` modules. The helpers below are neither, so the grant it \
              already makes for unit tests has to be restated for them."
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes projects built in the same process.
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A throwaway project on disk, removed on drop.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lanekeep-cli-{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

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
        std::fs::write(full, contents).expect("writes file");
    }

    fn check(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg(&self.dir)
            .args(args)
            .output()
            .expect("runs the binary")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Everything the binary wrote, with the exit code, for assertions and for failure output.
fn describe(output: &Output) -> String {
    format!(
        "exit: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

#[test]
fn a_built_in_rule_can_be_imported_by_specifier() {
    let project = Project::new(
        "builtin-import",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import noDefaultExport from 'lanekeep/no-default-export';\n\
                 export default defineConfig({ include: ['src/**'], rules: [noDefaultExport] });\n",
            ),
            ("src/a.ts", "export default function parse() {}\n"),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected violations to be found:\n{combined}"
    );
    assert!(
        combined.contains("lanekeep/no-default-export"),
        "the built-in did not report:\n{combined}"
    );
}

#[test]
fn a_built_in_factory_rule_can_be_configured() {
    let project = Project::new(
        "builtin-factory",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import noRestrictedImports from 'lanekeep/no-restricted-imports';\n\
                 export default defineConfig({\n\
                 \x20 include: ['src/**'],\n\
                 \x20 rules: [noRestrictedImports({ restrictions: [{ module: 'lodash', reason: 'use the standard library' }] })],\n\
                 });\n",
            ),
            (
                "src/a.ts",
                "import merge from 'lodash';\nexport { merge };\n",
            ),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected violations to be found:\n{combined}"
    );
    assert!(
        combined.contains("use the standard library"),
        "the configured reason did not reach the output:\n{combined}"
    );
}

#[test]
fn an_unknown_built_in_names_itself_in_the_error() {
    let project = Project::new(
        "builtin-unknown",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import rule from 'lanekeep/no-such-rule';\n\
                 export default defineConfig({ include: ['src/**'], rules: [rule] });\n",
            ),
            ("src/a.ts", "export const a = 1;\n"),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a config that cannot load is a runtime error, not a clean run:\n{combined}"
    );
    assert!(
        combined.contains("lanekeep/no-such-rule"),
        "the error does not name the specifier:\n{combined}"
    );
}

#[test]
fn a_project_file_cannot_shadow_a_built_in() {
    let project = Project::new(
        "builtin-shadow",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import noDefaultExport from 'lanekeep/no-default-export';\n\
                 export default defineConfig({ include: ['src/**'], rules: [noDefaultExport] });\n",
            ),
            // A rule that reports nothing, sitting exactly where naive path resolution
            // would look. If it won, the check below would pass with zero violations and
            // the tool would be silently disarmed.
            (
                "lanekeep/no-default-export.ts",
                "import { defineRule } from 'lanekeep';\n\
                 export default defineRule({\n\
                 \x20 id: 'local/impostor',\n\
                 \x20 severity: 'error',\n\
                 \x20 card: { message: 'never', remediation: 'never', examples: { bad: 'a', good: 'b' } },\n\
                 \x20 query: '(debugger_statement) @it',\n\
                 \x20 check() {},\n\
                 });\n",
            ),
            ("src/a.ts", "export default function parse() {}\n"),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "the real built-in should still have run:\n{combined}"
    );
    assert!(
        combined.contains("lanekeep/no-default-export"),
        "the built-in did not run:\n{combined}"
    );
    assert!(
        !combined.contains("local/impostor"),
        "a project file shadowed a built-in:\n{combined}"
    );
}

// --- built-ins that ship as components -----------------------------------------------

/// A built-in component, named exactly as `lanekeep init` scaffolds it.
///
/// **This is the only test that walks the whole chain in one process**: a `lanekeep.json` on
/// disk, parsed and resolved in Rust, its component bytes taken from the embedded table, asked
/// what it is, folded into `ruleset_hash`, compiled and executed over a real file. Every leg of
/// that has its own unit test and until this one nothing traversed it — the engine's component
/// tests hand it a `RuleSpec` built by hand, and the config crate's stop at a byte comparison.
#[test]
fn a_built_in_component_can_be_named_by_specifier() {
    let project = Project::new(
        "builtin-component",
        &[
            (
                "lanekeep.json",
                r#"{"include": ["src/**"], "rules": ["lanekeep/no-unwrap"]}"#,
            ),
            ("src/a.rs", "fn f() {\n    let c = load().unwrap();\n}\n"),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(1),
        "expected violations to be found:\n{combined}"
    );
    assert!(
        combined.contains("lanekeep/no-unwrap"),
        "the built-in component did not report:\n{combined}"
    );
}

/// And it is configurable, through the same specifier.
///
/// A component cannot close over a host-supplied value, so its options cross the boundary as
/// data — read from the JSON here, serialized once by `lanekeep-config`, handed to `configure`
/// on every instance. Asserting the *absence* of a violation is what makes this discriminating:
/// a rule whose options never arrived reports, and reporting is what the test above asserts.
#[test]
fn a_built_in_component_can_be_configured() {
    let project = Project::new(
        "builtin-component-configured",
        &[
            (
                "lanekeep.json",
                r#"{"include": ["src/**"],
                    "rules": [{"rule": "lanekeep/no-unwrap", "options": {"allow": ["src/a.rs"]}}]}"#,
            ),
            ("src/a.rs", "fn f() {\n    let c = load().unwrap();\n}\n"),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "the allow option did not reach the component:\n{combined}"
    );
}

/// A TypeScript config cannot import one, and is told why.
///
/// The one capability the swap costs, pinned so it stays a stated limit rather than a surprise.
/// A component has no JavaScript to evaluate and its identity comes from its own `metadata`
/// export, so there is nothing for an `import` to bind. What must not happen is the message
/// this would otherwise get — "no built-in rule by that name" — which is what a typo looks like
/// and would send its author hunting for a misspelling that is not there.
///
/// **Asserted on the first line only.** QuickJS truncates a thrown error at 256 bytes and
/// prefixes a resolution failure with the importing module's absolute path, which here is a
/// temporary directory over a hundred characters long. So the remedy — naming `lanekeep.json` —
/// is not asserted here; it is asserted where the message is not truncated, in `lanekeep-js`'s
/// `a_built_in_that_is_a_component_is_refused_as_a_module`.
#[test]
fn a_typescript_config_cannot_import_a_built_in_component() {
    let project = Project::new(
        "builtin-component-imported",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import noUnwrap from 'lanekeep/no-unwrap';\n\
                 export default defineConfig({ include: ['src/**'], rules: [noUnwrap] });\n",
            ),
            ("src/a.rs", "fn f() {\n    let c = load().unwrap();\n}\n"),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a config that cannot load is a runtime error, not a clean run:\n{combined}"
    );
    assert!(
        combined.contains("lanekeep/no-unwrap"),
        "the error does not name the specifier:\n{combined}"
    );
    assert!(
        combined.contains("it is a rule component, not a module"),
        "the error must say what the rule is, not that it is missing:\n{combined}"
    );
    assert!(
        !combined.contains("no built-in rule by that name"),
        "a component-backed built-in is not a typo, and must not be reported as one:\n{combined}"
    );
}

#[test]
fn the_rules_command_lists_a_built_in() {
    let project = Project::new(
        "builtin-listed",
        &[(
            "lanekeep.config.ts",
            "import { defineConfig } from 'lanekeep';\n\
             import noDefaultExport from 'lanekeep/no-default-export';\n\
             export default defineConfig({ include: ['src/**'], rules: [noDefaultExport] });\n",
        )],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_lanekeep"))
        .arg("rules")
        .arg(&project.dir)
        .output()
        .expect("runs the binary");

    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(0), "{combined}");
    assert!(
        combined.contains("lanekeep/no-default-export"),
        "the built-in is not listed:\n{combined}"
    );
}

#[test]
fn a_clean_project_using_built_ins_exits_zero() {
    let project = Project::new(
        "builtin-clean",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import noDefaultExport from 'lanekeep/no-default-export';\n\
                 export default defineConfig({ include: ['src/**'], rules: [noDefaultExport] });\n",
            ),
            ("src/a.ts", "export function parse() {}\n"),
        ],
    );

    let output = project.check(&[]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
}

#[test]
fn built_ins_are_reachable_without_a_lanekeep_directory() {
    // Nothing is written to disk to make a built-in resolve. A project that has never seen
    // `npm install` must be able to use them.
    let project = Project::new(
        "builtin-no-dir",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import noDefaultExport from 'lanekeep/no-default-export';\n\
                 export default defineConfig({ include: ['src/**'], rules: [noDefaultExport] });\n",
            ),
            ("src/a.ts", "export default 1;\n"),
        ],
    );
    assert!(
        !Path::new(&project.dir).join("lanekeep").exists(),
        "the fixture accidentally created a lanekeep directory"
    );

    let output = project.check(&[]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
}
