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

/// A built-in that still ships as a TypeScript module, imported the way a `.config.ts` does.
///
/// Python rather than TypeScript, and that is not arbitrary: after the four flagship TypeScript
/// rules were compiled into a component, every built-in that is still a *module* targets Python,
/// Go or Rust. The import path is what is under test, so the rule has to be one that can still
/// be imported — and a test that quietly switched to a `lanekeep.json` to keep passing would
/// have stopped covering this path at all while looking exactly as green.
#[test]
fn a_built_in_rule_can_be_imported_by_specifier() {
    let project = Project::new(
        "builtin-import",
        &[
            (
                "lanekeep.config.ts",
                "import { defineConfig } from 'lanekeep';\n\
                 import noBroadExcept from 'lanekeep/no-broad-except';\n\
                 export default defineConfig({ include: ['src/**'], rules: [noBroadExcept] });\n",
            ),
            ("src/a.py", "try:\n    run()\nexcept Exception:\n    pass\n"),
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
        combined.contains("lanekeep/no-broad-except"),
        "the built-in did not report:\n{combined}"
    );
}

/// A built-in **factory**, configured — through the format that can now reach one.
///
/// `no-restricted-imports` exports a function over its options rather than a rule object, and
/// both shapes have to keep working now that they live in one component: `configure` applies a
/// factory to its options and uses a rule object as it comes. This is the factory half, end to
/// end, from JSON on disk to a message carrying the configured reason.
///
/// It was a `lanekeep.config.ts` calling `noRestrictedImports({...})` until this rule became a
/// component, and there is no remaining built-in factory that a `.config.ts` can import — the
/// four modules that are left all export a rule object. So the capability being covered here is
/// "a built-in factory can be configured", and the format it is covered through moved because
/// the rule did.
#[test]
fn a_built_in_factory_rule_can_be_configured() {
    let project = Project::new(
        "builtin-factory",
        &[
            (
                "lanekeep.json",
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": [{"rule": "lanekeep/no-restricted-imports",
                               "options": {"restrictions": [
                                   {"module": "lodash",
                                    "reason": "use the standard library"}]}}]}"#,
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

/// And a built-in that is **not** a factory refuses options rather than ignoring them.
///
/// `no-default-export` exports a rule object, which has nowhere to put options: they reach a
/// rule by being closed over, and only a factory closes over anything. Silently discarding them
/// is the failure `AGENTS.md` records against `no-unwrap` and `no-glob-import`, whose documented
/// `allow` option was dead code from the day each shipped — invisible because an ignored option
/// only ever *adds* violations, so a user seeing one assumes their pattern is wrong.
///
/// Exit 2 and the rule's id in the message. A run that merely reported more than it should have
/// is the shape this is written to prevent.
#[test]
fn a_built_in_that_is_not_a_factory_refuses_options() {
    let project = Project::new(
        "builtin-not-a-factory",
        &[
            (
                "lanekeep.json",
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": [{"rule": "lanekeep/no-default-export",
                               "options": {"allow": ["src/a.ts"]}}]}"#,
            ),
            ("src/a.ts", "export default function parse() {}\n"),
        ],
    );

    let output = project.check(&[]);
    let combined = describe(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "options a rule cannot use are a misconfiguration, not a violation:\n{combined}"
    );
    assert!(
        combined.contains("lanekeep/no-default-export"),
        "the refusal has to name the rule that was misconfigured:\n{combined}"
    );
    assert!(
        combined.contains("takes no options"),
        "the refusal has to say what is wrong with the config:\n{combined}"
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
                "lanekeep.json",
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": ["lanekeep/no-default-export"]}"#,
            ),
            // A rule that reports nothing, sitting exactly where naive path resolution
            // would look. If it won, the check below would pass with zero violations and
            // the tool would be silently disarmed.
            //
            // A component-backed built-in is the sharper version of this: the impostor is a
            // module and the real rule is not, so a resolver that fell back to the filesystem
            // would find something that loads.
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
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": ["lanekeep/no-unwrap"]}"#,
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
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
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
/// **The remedy is asserted here, and it is the half worth asserting.** QuickJS truncates a
/// thrown error at 255 bytes and rquickjs spends the front of that on the importing module's
/// absolute path, which is what makes this the realistic test rather than the unit one. An
/// earlier two-line version of the message lost "name it in a `lanekeep.json`" to exactly this
/// path, and this test passed, because it only checked the first line. Telling a user their
/// config is wrong without telling them the one thing that fixes it is barely better than the
/// "no built-in rule by that name" this replaced.
///
/// # Both ends of the name-length budget, and why the fixture names are as short as they are
///
/// The rule's name is spent twice inside those 255 bytes — once in rquickjs's framing, once in
/// the message — so the *longest* name that ships is the case that can fail, and `no-unwrap` at
/// nine characters would pass with headroom `no-restricted-imports` does not have. Both are
/// covered.
///
/// What is left for the path is `150 - 2 * name`, which
/// `lanekeep_js::MAX_COMPONENT_NAME` derives and states as 108 bytes at the current longest
/// name. **A temporary directory is not a project path**: macOS's is 56 bytes of
/// `/private/var/folders/…/T/` before this fixture writes anything, so a descriptive directory
/// name here spends the user's budget on the harness and asserts something no config path has
/// to satisfy. The name is one character for that reason, and the failure below prints the
/// length it actually had, so a machine with an unusually long `TMPDIR` says so rather than
/// reading as a regression in the message.
#[allow(clippy::single_element_loop)]
#[test]
fn a_typescript_config_cannot_import_a_built_in_component() {
    for (name, subject, source) in [(
        "no-unwrap",
        "src/a.rs",
        "fn f() {\n    let c = load().unwrap();\n}\n",
    )] {
        let project = Project::new(
            "c",
            &[
                (
                    "lanekeep.config.ts",
                    &format!(
                        "import {{ defineConfig }} from 'lanekeep';\n\
                         import rule from 'lanekeep/{name}';\n\
                         export default defineConfig({{ include: ['src/**'], rules: [rule] }});\n"
                    ),
                ),
                (subject, source),
            ],
        );

        // What rquickjs will actually put in front of the message: the *canonical* path, which
        // on macOS is eight bytes longer than the one the fixture built.
        let config_path = std::fs::canonicalize(&project.dir)
            .unwrap_or_else(|_| project.dir.clone())
            .join("lanekeep.config.ts");
        let path_bytes = config_path.as_os_str().len();

        let output = project.check(&[]);
        let combined = describe(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "a config that cannot load is a runtime error, not a clean run:\n{combined}"
        );
        assert!(
            combined.contains(&format!("lanekeep/{name}")),
            "the error does not name the specifier:\n{combined}"
        );
        assert!(
            combined.contains("is a rule component"),
            "the error must say what the rule is, not that it is missing:\n{combined}"
        );
        assert!(
            combined.contains("name it in a `lanekeep.json`"),
            "the remedy has to survive the truncation, or the error is not actionable\n  \
             the config path was {path_bytes} bytes and the budget for `{name}` is \
             {} — a path over that is this machine's `TMPDIR`, not a regression here\n{combined}",
            150 - 2 * name.len(),
        );
        assert!(
            !combined.contains("no built-in rule by that name"),
            "a component-backed built-in is not a typo, and must not be reported as one:\n{combined}"
        );
    }
}

#[test]
fn the_rules_command_lists_a_built_in() {
    let project = Project::new(
        "builtin-listed",
        &[(
            "lanekeep.json",
            r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": ["lanekeep/no-default-export"]}"#,
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
                "lanekeep.json",
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": ["lanekeep/no-default-export"]}"#,
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
                "lanekeep.json",
                r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
                    "rules": ["lanekeep/no-default-export"]}"#,
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
