//! `--write-baseline` and `--baseline`, driven through the binary (issue #280).
//!
//! Every behavior the issue names has a test here: a moved violation stays baselined, a second
//! copy is new, the exit code follows new violations only, entries that no longer occur are
//! listed so the file can shrink, and `--since` composes without calling an unchecked file's
//! entries stale.

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

const CONFIG: &str = r#"{"include": ["src/**"], "timeouts": {"rule": 600000, "global": 600000},
     "rules": ["lanekeep/no-default-export"]}"#;

/// A project directory with a lanekeep config, optionally a git repository, removed on drop.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lanekeep-baseline-{name}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates dir");
        let project = Self { dir };
        project.write("lanekeep.json", CONFIG);
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

    /// The baseline's path, absolute: the flag resolves against the cwd, like `--config`.
    fn baseline(&self) -> String {
        self.dir.join("baseline.json").display().to_string()
    }

    fn git(&self, args: &[&str]) {
        // `-C` does not override an inherited `GIT_DIR` (AGENTS.md: the `core.bare` trap).
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("runs git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git(&self) {
        self.git(&["init", "--quiet"]);
        self.git(&["config", "user.email", "test@example.com"]);
        self.git(&["config", "user.name", "Test"]);
        self.git(&["config", "commit.gpgsign", "false"]);
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", "first"]);
    }

    fn check(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lanekeep"))
            .arg("check")
            .arg(&self.dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("runs the binary")
    }

    /// Run with `--format json` and the given flags; the reported violations as `file:line`.
    fn reported(&self, args: &[&str]) -> (Output, Vec<String>) {
        let mut all = vec!["--format", "json"];
        all.extend_from_slice(args);
        let output = self.check(&all);
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("stdout is not a JSON document ({e})\n{}", describe(&output)))
            .expect("stdout is a JSON document");
        let reported = document["violations"]
            .as_array()
            .expect("a violations array")
            .iter()
            .map(|v| {
                format!(
                    "{}:{}",
                    v["location"]["file"].as_str().expect("a file"),
                    v["location"]["position"]["line"]
                )
            })
            .collect();
        (output, reported)
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn write_then_check_is_clean() {
    let project = Project::new(
        "clean",
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );
    let baseline = project.baseline();

    let written = project.check(&["--write-baseline", &baseline]);
    assert_eq!(written.status.code(), Some(0), "{}", describe(&written));
    assert!(
        stderr(&written).contains("2 violation(s)"),
        "{}",
        describe(&written)
    );

    let (output, reported) = project.reported(&["--baseline", &baseline]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(reported.is_empty(), "{}", describe(&output));
    // Hiding is said out loud: a silent baseline reads as "clean".
    assert!(
        stderr(&output).contains("2 violation(s) matched the baseline"),
        "{}",
        describe(&output)
    );
}

#[test]
fn lines_inserted_above_stay_baselined() {
    let project = Project::new("moved", &[("src/a.ts", "export default 1;\n")]);
    let baseline = project.baseline();
    let written = project.check(&["--write-baseline", &baseline]);
    assert_eq!(written.status.code(), Some(0), "{}", describe(&written));

    project.write(
        "src/a.ts",
        "// a comment\nconst x = 1;\n\n    export   default 1;\n",
    );
    let (output, reported) = project.reported(&["--baseline", &baseline]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(reported.is_empty(), "{}", describe(&output));
}

#[test]
fn a_new_violation_fails_and_only_it_is_reported() {
    let project = Project::new("new", &[("src/a.ts", "export default 1;\n")]);
    let baseline = project.baseline();
    let written = project.check(&["--write-baseline", &baseline]);
    assert_eq!(written.status.code(), Some(0), "{}", describe(&written));

    project.write("src/c.ts", "export default 3;\n");
    let (output, reported) = project.reported(&["--baseline", &baseline]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert_eq!(reported, ["src/c.ts:1"], "{}", describe(&output));
}

#[test]
fn a_second_copy_of_a_baselined_violation_is_new() {
    // `no-default-export` reports each default export, and a module may carry two only by
    // mistake — but the engine reports both, which is what this needs: two identical lines.
    let project = Project::new("copy", &[("src/a.ts", "export default 1;\n")]);
    let baseline = project.baseline();
    let written = project.check(&["--write-baseline", &baseline]);
    assert_eq!(written.status.code(), Some(0), "{}", describe(&written));

    project.write("src/a.ts", "export default 1;\nexport default 1;\n");
    let (output, reported) = project.reported(&["--baseline", &baseline]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert_eq!(reported, ["src/a.ts:2"], "{}", describe(&output));
}

#[test]
fn a_fixed_violation_is_listed_as_stale_and_passes() {
    let project = Project::new(
        "stale",
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );
    let baseline = project.baseline();
    let written = project.check(&["--write-baseline", &baseline]);
    assert_eq!(written.status.code(), Some(0), "{}", describe(&written));

    project.write("src/a.ts", "export const a = 1;\n");
    let (output, reported) = project.reported(&["--baseline", &baseline]);
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(reported.is_empty(), "{}", describe(&output));
    let err = stderr(&output);
    assert!(
        err.contains("1 baseline entr") && err.contains("lanekeep/no-default-export src/a.ts"),
        "{}",
        describe(&output)
    );
    assert!(!err.contains("src/b.ts"), "{}", describe(&output));
    assert!(err.contains("--write-baseline"), "{}", describe(&output));
}

#[test]
fn since_reports_new_and_never_calls_an_unchanged_files_entry_stale() {
    let project = Project::new(
        "since",
        &[
            ("src/a.ts", "export default 1;\n"),
            ("src/b.ts", "export default 2;\n"),
        ],
    );
    let baseline = project.baseline();
    let written = project.check(&["--write-baseline", &baseline]);
    assert_eq!(written.status.code(), Some(0), "{}", describe(&written));
    project.init_git();

    // Only `src/a.ts` changes: its baselined violation moves down a line and a new one
    // appears. `src/b.ts` is outside the selection, so its entry is not checked at all.
    project.write(
        "src/a.ts",
        "export const z = 0;\nexport default 1;\nexport default 9;\n",
    );
    let (output, reported) = project.reported(&["--since", "HEAD", "--baseline", &baseline]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert_eq!(reported, ["src/a.ts:3"], "{}", describe(&output));
    assert!(
        !stderr(&output).contains("no longer occur"),
        "{}",
        describe(&output)
    );

    // And a fix inside the selection is stale, while the unselected file still is not.
    project.write("src/a.ts", "export const z = 0;\n");
    let (output, _) = project.reported(&["--since", "HEAD", "--baseline", &baseline]);
    let err = stderr(&output);
    assert!(err.contains("src/a.ts"), "{}", describe(&output));
    assert!(!err.contains("src/b.ts"), "{}", describe(&output));
}

#[test]
fn write_baseline_refuses_a_narrowed_selection() {
    // A baseline written from a subset would silently drop every other file's entries.
    let project = Project::new("narrowed", &[("src/a.ts", "export default 1;\n")]);
    let baseline = project.baseline();
    for narrowing in [
        &["--staged"][..],
        &["--since", "HEAD"][..],
        &["--file", "src/a.ts"][..],
    ] {
        let mut args = vec!["--write-baseline", baseline.as_str()];
        args.extend_from_slice(narrowing);
        let output = project.check(&args);
        assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
        assert!(
            stderr(&output).contains("cannot be used with"),
            "{}",
            describe(&output)
        );
    }
    assert!(!project.dir.join("baseline.json").exists());
}

#[test]
fn a_missing_baseline_is_a_runtime_error() {
    let project = Project::new("missing", &[("src/a.ts", "export default 1;\n")]);
    let output = project.check(&["--baseline", &project.baseline()]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(
        stderr(&output).contains("baseline.json"),
        "{}",
        describe(&output)
    );
}

#[test]
fn a_baseline_from_another_version_is_refused() {
    let project = Project::new("version", &[("src/a.ts", "export default 1;\n")]);
    project.write("baseline.json", r#"{"version": 2, "entries": []}"#);
    let output = project.check(&["--baseline", &project.baseline()]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(
        stderr(&output).contains("version 2"),
        "{}",
        describe(&output)
    );
}

#[test]
fn writing_twice_is_byte_identical() {
    let project = Project::new(
        "stable",
        &[
            ("src/b.ts", "export default 2;\n"),
            ("src/a.ts", "export default 1;\n"),
        ],
    );
    let baseline = project.baseline();
    let first = project.check(&["--write-baseline", &baseline]);
    assert_eq!(first.status.code(), Some(0), "{}", describe(&first));
    let once = std::fs::read(&baseline).expect("written");
    let second = project.check(&["--write-baseline", &baseline]);
    assert_eq!(second.status.code(), Some(0), "{}", describe(&second));
    let twice = std::fs::read(&baseline).expect("written");
    assert_eq!(once, twice);

    let text = String::from_utf8(once).expect("utf-8");
    assert!(text.ends_with("}\n"), "{text}");
    let a = text.find("src/a.ts").expect("names src/a.ts");
    let b = text.find("src/b.ts").expect("names src/b.ts");
    assert!(a < b, "entries are sorted: {text}");
}

#[test]
fn a_suppression_violation_is_never_baselined() {
    // Architecture §10: `lanekeep/suppression` cannot be suppressed, so a baseline cannot
    // waive it either — writing one still fails the run on a directive with no reason.
    // Split, or lanekeep's self-check reads this very line as a directive with no reason.
    const NEXT_LINE: &str = concat!("lanekeep", "-ignore-next-line");
    let source = format!("// {NEXT_LINE} lanekeep/no-default-export\nexport default 1;\n");
    let project = Project::new("suppression", &[("src/a.ts", &source)]);
    let baseline = project.baseline();
    let (written, reported) = project.reported(&["--write-baseline", &baseline]);
    assert_eq!(written.status.code(), Some(1), "{}", describe(&written));
    assert_eq!(reported, ["src/a.ts:1"], "{}", describe(&written));
    let recorded = std::fs::read_to_string(&baseline).expect("written");
    assert!(!recorded.contains("lanekeep/suppression"), "{recorded}");

    let (output, reported) = project.reported(&["--baseline", &baseline]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert_eq!(reported, ["src/a.ts:1"], "{}", describe(&output));
}
