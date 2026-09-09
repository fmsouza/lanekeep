//! `--file`, driven through the binary over a plain directory: no git involved.

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

/// A project directory with a lanekeep config, removed on drop.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, config: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lanekeep-file-{name}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates dir");
        let project = Self { dir };
        project.write("lanekeep.json", config);
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

    fn check(&self, args: &[&str]) -> Output {
        // No `.current_dir()` on purpose: the run inherits the harness's cwd (the crate
        // dir), which is not the project — that mismatch is what pins root-relative
        // resolution in `file_resolves_against_the_project_root_not_the_cwd`.
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

fn describe(output: &Output) -> String {
    format!(
        "exit: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// How many violations the run reported, from the human summary.
fn violation_count(output: &Output) -> usize {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| {
            line.split_once(" error(s)")
                .and_then(|(head, _)| head.rsplit(' ').next())
                .and_then(|n| n.parse::<usize>().ok())
        })
        .unwrap_or(0)
}

const PER_FILE_CONFIG: &str = r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
     "rules": ["lanekeep/no-default-export"]}"#;

const WIDE_CONFIG: &str = r#"{"include": ["**/*"], "timeouts": {"rule": 600000, "global": 600000},
     "rules": ["lanekeep/no-default-export"]}"#;

const CROSS_FILE_CONFIG: &str = r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
     "rules": [{"rule": "lanekeep/no-unused-exports", "options": {}}]}"#;

#[test]
fn file_checks_exactly_the_named_file() {
    let project = Project::new(
        "named",
        PER_FILE_CONFIG,
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );

    let output = project.check(&["--file", "src/a.ts"]);
    let combined = describe(&output);
    assert_eq!(violation_count(&output), 1, "{combined}");
    assert!(combined.contains("src/a.ts"), "{combined}");
    assert!(!combined.contains("src/b.ts"), "{combined}");
}

#[test]
fn file_is_repeatable() {
    let project = Project::new(
        "repeatable",
        PER_FILE_CONFIG,
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );

    let output = project.check(&["--file", "src/a.ts", "--file", "src/b.ts"]);
    assert_eq!(violation_count(&output), 2, "{}", describe(&output));
}

#[test]
fn file_resolves_against_the_project_root_not_the_cwd() {
    // The run's cwd is the crate dir, not the project: resolving the argument against
    // anything else fails to find the file and exits 2.
    let project = Project::new(
        "root-relative",
        PER_FILE_CONFIG,
        &[("src/a.ts", "export default 1;\n")],
    );
    let output = project.check(&["--file", "src/a.ts"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(1), "{combined}");
    assert_eq!(violation_count(&output), 1, "{combined}");
}

#[test]
fn a_file_outside_include_is_an_error() {
    let project = Project::new(
        "outside-include",
        PER_FILE_CONFIG,
        &[
            ("other/d.ts", "export default 1;\n"),
            ("src/a.ts", "const a = 1;\n"),
        ],
    );
    let output = project.check(&["--file", "other/d.ts"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains("--file other/d.ts"), "{combined}");
    assert!(combined.contains("include"), "{combined}");
}

#[test]
fn an_excluded_file_is_an_error_naming_the_pattern() {
    let config = r#"{"include": ["**/*"], "exclude": ["vendor/**"], "timeouts": {"rule": 600000, "global": 600000},
         "rules": ["lanekeep/no-default-export"]}"#;
    let project = Project::new(
        "excluded-file",
        config,
        &[
            ("vendor/x.ts", "export default 1;\n"),
            ("src/a.ts", "const a = 1;\n"),
        ],
    );
    let output = project.check(&["--file", "vendor/x.ts"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains("exclude"), "{combined}");
    assert!(combined.contains("vendor/**"), "{combined}");
}

#[test]
fn a_gitignored_file_is_an_error() {
    // No glob rejects it — the walk does. The one rejection only the walk can see, and
    // the reason the error has to name rather than guessing a glob clause.
    let project = Project::new(
        "gitignored",
        WIDE_CONFIG,
        &[
            ("dist/b.ts", "export default 1;\n"),
            ("src/a.ts", "const a = 1;\n"),
        ],
    );
    project.write(".gitignore", "dist/\n");
    let output = project.check(&["--file", "dist/b.ts"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains(".gitignore"), "{combined}");
}

#[test]
fn lanekeeps_own_directory_is_an_error() {
    // `**/*` says everything, but `.lanekeep/` is not configurable: nothing can include it.
    let project = Project::new(
        "own-dir",
        WIDE_CONFIG,
        &[
            (".lanekeep/driver.mjs", "const x = 1;\n"),
            ("src/a.ts", "const a = 1;\n"),
        ],
    );
    let output = project.check(&["--file", ".lanekeep/driver.mjs"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains("lanekeep's own directory"), "{combined}");
}

#[test]
fn a_missing_file_is_an_error() {
    // Checking everything instead would be surprising work done silently; checking
    // nothing would look like a clean run. The same reasoning as an unknown ref.
    let project = Project::new(
        "missing",
        PER_FILE_CONFIG,
        &[("src/a.ts", "const a = 1;\n")],
    );
    let output = project.check(&["--file", "src/nope.ts"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains("no such file"), "{combined}");
}

#[test]
fn a_file_outside_the_project_root_is_an_error() {
    // A real sibling file, so canonicalize succeeds and only the root check can fail.
    let project = Project::new(
        "outside-root",
        PER_FILE_CONFIG,
        &[("src/a.ts", "const a = 1;\n")],
    );
    // Named from the project dir like `Project::new` names its own, so two concurrent
    // runs of this suite cannot race one shared sibling into a "no such file" flake.
    let outside = project
        .dir
        .with_file_name(format!("lanekeep-237-sibling-{}.ts", std::process::id()));
    std::fs::write(&outside, "const x = 1;\n").expect("writes the sibling");
    let output = project.check(&[
        "--file",
        &format!("../lanekeep-237-sibling-{}.ts", std::process::id()),
    ]);
    let _ = std::fs::remove_file(&outside);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(
        combined.contains("not under the project root"),
        "{combined}"
    );
}

#[test]
fn a_directory_is_an_error() {
    // Expanding a directory is deferred; naming one is refused rather than guessed at.
    let project = Project::new(
        "directory",
        PER_FILE_CONFIG,
        &[("src/a.ts", "const a = 1;\n")],
    );
    let output = project.check(&["--file", "src"]);
    let combined = describe(&output);
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(combined.contains("directory"), "{combined}");
}

#[test]
fn file_and_since_cannot_be_combined() {
    // Same shape as --since + --staged: two ways to narrow are one, decided by clap.
    let project = Project::new(
        "file-since",
        PER_FILE_CONFIG,
        &[("src/a.ts", "const a = 1;\n")],
    );
    let output = project.check(&["--file", "src/a.ts", "--since", "HEAD"]);
    let combined = describe(&output);
    assert_ne!(output.status.code(), Some(0), "{combined}");
    assert!(combined.contains("cannot be used with"), "{combined}");
}

#[test]
fn file_and_staged_cannot_be_combined() {
    let project = Project::new(
        "file-staged",
        PER_FILE_CONFIG,
        &[("src/a.ts", "const a = 1;\n")],
    );
    let output = project.check(&["--file", "src/a.ts", "--staged"]);
    let combined = describe(&output);
    assert_ne!(output.status.code(), Some(0), "{combined}");
    assert!(combined.contains("cannot be used with"), "{combined}");
}

#[test]
fn a_file_selection_skips_cross_file_rules_and_says_so() {
    // The note is a property of narrowing, not of git: the flag it names is `--file`.
    let project = Project::new(
        "file-cross-file",
        CROSS_FILE_CONFIG,
        &[
            (
                "src/a.ts",
                "export function used() {}\nexport function spare() {}\n",
            ),
            ("src/b.ts", "import { used } from './a';\nused();\n"),
        ],
    );
    let output = project.check(&["--file", "src/a.ts"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lanekeep/no-unused-exports"),
        "the skipped rule is not named: {stderr}"
    );
    assert!(stderr.contains("--file"), "{stderr}");
    assert_eq!(
        violation_count(&output),
        0,
        "the cross-file rule should not have reported: {}",
        describe(&output)
    );
}
